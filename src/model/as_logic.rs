// File: src/model/as_logic.rs
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use crate::math::volatility::RollingVolatility;
use crate::core::Side;
use serde::{Serialize, Deserialize};
use std::sync::mpsc::Sender;

// --- 配置部分 (保持不变) ---
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub risk_aversion_gamma: f64,
    pub liquidity_k: f64,
    pub min_spread_bps: u32,
    pub tick_size: f64,
    pub max_inventory_usd: f64,
    pub default_order_size_usd: f64,
    pub vol_window_size: usize,
    pub maturity_timestamp_ms: i64,
    pub terminal_dumping_factor: f64,
    pub closing_window_seconds: i64,
}

// --- 持久化状态结构 ---
#[derive(Debug, Serialize, Deserialize)]
pub struct PersistState {
    pub realized_inventory: f64, // 改名：只存确认的
    pub cash_balance: f64,
    pub timestamp: i64,
}

pub struct OpinionGridStrategy {
    pub cfg: StrategyConfig,
    vol_calc: RollingVolatility,
    
    // --- [核心修改] 双层库存系统 ---
    /// 1. 链上/API 确认的真实库存 (Source of Truth)
    pub realized_inventory: f64,
    
    /// 2. 乐观预估的在途库存 (Pending)
    /// 发单瞬间 +1，收到成交瞬间 -1。
    /// 正数代表有“买单在途”，负数代表“卖单在途”
    pub pending_inventory: f64,

    /// 账户里的现金余额 (Realized PnL 累积)
    pub current_cash_balance: f64,
    
    // 辅助状态
    last_equity_mark: f64, 
    persist_sender: Option<Sender<PersistState>>, 
}

impl OpinionGridStrategy {
    pub fn new(cfg: StrategyConfig, sender: Option<Sender<PersistState>>) -> Self {
        let win_size = cfg.vol_window_size;
        Self {
            cfg,
            vol_calc: RollingVolatility::new(win_size),
            realized_inventory: 0.0,
            pending_inventory: 0.0,
            current_cash_balance: 0.0,
            last_equity_mark: 0.0,
            persist_sender: sender,
        }
    }

    /// [系统启动] 恢复状态
    /// 注意：启动时 pending 必须归零，因为我们不知道之前的挂单是否还活着
    pub fn restore_state(&mut self, saved_inv: f64, saved_cash: f64) {
        self.realized_inventory = saved_inv;
        self.current_cash_balance = saved_cash;
        self.pending_inventory = 0.0; 
        println!("♻️ [State Restored] Realized Inv: {}, Cash: ${:.4}", saved_inv, saved_cash);
    }

    /// [核心 Getter] 获取用于计算 Skew 的“有效库存”
    /// Effective = Realized (手里的) + Pending (即将到手的)
    pub fn get_effective_inventory(&self) -> f64 {
        self.realized_inventory + self.pending_inventory
    }

    // --- 事件处理 A: 乐观发单 (Optimistic Update) ---
    // 在 Engine 决定发单的那一刻调用
    pub fn on_signal_created(&mut self, side: Side, price: Decimal, size_usd: Decimal) {
        let price_f64 = price.to_f64().unwrap_or(0.0);
        if price_f64 <= 0.0 { return; }

        let size_f64 = size_usd.to_f64().unwrap_or(0.0);
        let estimated_shares = size_f64 / price_f64;

        // 更新在途库存
        match side {
            Side::Buy => self.pending_inventory += estimated_shares,
            Side::Sell => self.pending_inventory -= estimated_shares,
        }

        // 打印日志方便调试
        // println!("🚀 [Optimistic] Pending: {:.2} (Eff: {:.2})", self.pending_inventory, self.get_effective_inventory());
    }

    // --- 事件处理 B: 确认成交 (Confirmed Fill) ---
    // 当从 WebSocket/API 收到 InventoryUpdate 时调用
    pub fn on_fill_confirmed(&mut self, change_shares: f64, net_cash_flow: f64) {
        // 1. 更新真实库存
        self.realized_inventory += change_shares;
        self.current_cash_balance += net_cash_flow;

        // 2. 核销 Pending (关键!)
        // 如果我们买入了 10 个，说明之前的“买入预期”兑现了，需要从 pending 里扣除 10 个
        // 这样 Total Effective Inventory 保持不变（平滑过渡）
        self.pending_inventory -= change_shares;

        // 3. Pending 漂移修正 (可选但推荐)
        // 防止由于丢包或精度问题导致 pending 永远不归零
        // 这里做一个简单的衰减：每次成交后，把 pending 向 0 压缩 1%
        self.pending_inventory *= 0.99; 
        // 或者硬性修正：如果绝对值很小，直接归零
        if self.pending_inventory.abs() < 0.1 {
            self.pending_inventory = 0.0;
        }

        // 4. 触发持久化
        self.persist();
    }

    // 辅助: 写入磁盘
    fn persist(&self) {
        if let Some(tx) = &self.persist_sender {
            let _ = tx.send(PersistState {
                realized_inventory: self.realized_inventory, // 存的是真实库存
                cash_balance: self.current_cash_balance,
                timestamp: chrono::Utc::now().timestamp(),
            });
        }
    }

    /// [核心计算] 这里的公式必须使用 get_effective_inventory()
    pub fn calculate_quotes(&mut self, poly_mid_price: Decimal) -> (Decimal, Decimal) {
        let now = chrono::Utc::now().timestamp_millis();
        let time_left_ms = self.cfg.maturity_timestamp_ms - now;
        
        if time_left_ms <= 0 { return (dec!(0), dec!(0)); }

        let t_days = time_left_ms as f64 / (1000.0 * 3600.0 * 24.0);

        // 动态 Gamma
        let effective_gamma = if time_left_ms < (self.cfg.closing_window_seconds * 1000) {
            let progress = 1.0 - (time_left_ms as f64 / (self.cfg.closing_window_seconds * 1000.0 as f64));
            self.cfg.risk_aversion_gamma * (1.0 + progress * self.cfg.terminal_dumping_factor)
        } else {
            self.cfg.risk_aversion_gamma
        };

        let sigma = self.vol_calc.update(poly_mid_price);
        let mid_f64 = poly_mid_price.to_f64().unwrap_or(0.5);
        
        // --- 关键点：使用 Effective Inventory ---
        let current_inv = self.get_effective_inventory();
        
        // AS 模型核心公式
        let risk_term = current_inv * effective_gamma * (sigma * sigma) * t_days.max(0.01); 
        let reservation_price = mid_f64 - risk_term;

        let spread_term_1 = effective_gamma * (sigma * sigma) * t_days.max(0.01);
        let spread_term_2 = (2.0 / effective_gamma) * (1.0 + effective_gamma / self.cfg.liquidity_k).ln();
        
        let half_spread = spread_term_1 + spread_term_2;
        let min_half = (self.cfg.min_spread_bps as f64 / 10000.0) / 2.0;
        let final_half_spread = half_spread.max(min_half);

        let raw_bid = reservation_price - final_half_spread;
        let raw_ask = reservation_price + final_half_spread;

        (
            Self::round_to_tick(raw_bid, self.cfg.tick_size),
            Self::round_to_tick(raw_ask, self.cfg.tick_size)
        )
    }

    // PnL 计算也要用 Realized 还是 Effective? 
    // 风控通常看 Realized (因为那是真正会爆仓的)，但预估回撤可以用 Effective
    pub fn calculate_equity_change(&mut self, current_mid_price: f64) -> f64 {
        // 这里保守一点，使用 Realized 计算资金权益，因为 pending 还没扣钱
        let position_value = self.realized_inventory * current_mid_price;
        let current_equity = self.current_cash_balance + position_value;

        if self.last_equity_mark == 0.0 {
             self.last_equity_mark = current_equity;
             return 0.0;
        }

        let pnl_change = current_equity - self.last_equity_mark;
        self.last_equity_mark = current_equity;
        
        pnl_change
    }

    fn round_to_tick(price: f64, tick: f64) -> Decimal {
        let p = (price / tick).round() * tick;
        // 兜底防止价格越界
        let clamped = p.max(0.001).min(0.999);
        Decimal::from_f64_retain(clamped).unwrap_or(dec!(0.5))
    }
}