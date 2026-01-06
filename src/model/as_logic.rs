// File: src/model/as_logic.rs
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use crate::math::volatility::RollingVolatility;
use serde::{Serialize, Deserialize};
use std::sync::mpsc::Sender;

// --- 配置部分 ---
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub risk_aversion_gamma: f64,
    pub liquidity_k: f64,
    pub min_spread_bps: u32,
    pub tick_size: f64,
    pub max_inventory_usd: f64,
    // 时间相关
    pub maturity_timestamp_ms: i64,
    pub terminal_dumping_factor: f64,
    pub closing_window_seconds: i64,
}

// --- 持久化状态结构 (写入磁盘的内容) ---
#[derive(Debug, Serialize, Deserialize)]
pub struct PersistState {
    pub inventory_shares: f64,
    pub cash_balance: f64, // 账户里的现金余额 (Realized PnL 累积)
    pub timestamp: i64,
}

pub struct OpinionGridStrategy {
    cfg: StrategyConfig,
    vol_calc: RollingVolatility,
    
    // 核心状态
    pub current_inventory_shares: f64,
    pub current_cash_balance: f64, // 内存中的现金余额
    
    // 辅助状态：用于计算权益变动
    last_equity_mark: f64, 
    
    // IO 通道
    persist_sender: Option<Sender<PersistState>>, 
}

impl OpinionGridStrategy {
    pub fn new(cfg: StrategyConfig, sender: Option<Sender<PersistState>>) -> Self {
        Self {
            cfg,
            vol_calc: RollingVolatility::new(100),
            current_inventory_shares: 0.0,
            current_cash_balance: 0.0, // 初始为 0，等待 restore
            last_equity_mark: 0.0,
            persist_sender: sender,
        }
    }

    /// [系统启动时调用] 恢复之前的账本
    pub fn restore_state(&mut self, saved_inv: f64, saved_cash: f64) {
        self.current_inventory_shares = saved_inv;
        self.current_cash_balance = saved_cash;
        println!("♻️ [State Restored] Inv: {}, Cash: ${:.4}", saved_inv, saved_cash);
    }

    /// [成交回调] 更新库存和现金，并触发异步写入
    pub fn on_fill(&mut self, change_shares: f64, net_cash_flow: f64) {
        self.current_inventory_shares += change_shares;
        self.current_cash_balance += net_cash_flow;

        // ⚡️ 异步 IO：状态存盘
        if let Some(tx) = &self.persist_sender {
            // 这里我们忽略 send 错误，因为在极高频下如果 channel 满了，我们选择丢弃旧状态
            // 但对于资金状态，最好保证 buffer 足够大
            let _ = tx.send(PersistState {
                inventory_shares: self.current_inventory_shares,
                cash_balance: self.current_cash_balance,
                timestamp: chrono::Utc::now().timestamp(),
            });
        }
    }

    /// [核心风控计算] 计算权益变动 (Mark-to-Market PnL)
    /// 公式：Total Equity = Cash + (Inventory * MidPrice)
    /// 返回值：PnL Change (相对于上一次计算的变动值)
    pub fn calculate_equity_change(&mut self, current_mid_price: f64) -> f64 {
        let position_value = self.current_inventory_shares * current_mid_price;
        let current_equity = self.current_cash_balance + position_value;

        // 如果是启动后的第一次计算，我们初始化基准值，不产生 PnL 跳变
        if self.last_equity_mark == 0.0 && self.current_inventory_shares == 0.0 && self.current_cash_balance == 0.0 {
             self.last_equity_mark = current_equity;
             return 0.0;
        }
        
        // 第一次恢复状态后的校准
        if self.last_equity_mark == 0.0 {
             self.last_equity_mark = current_equity;
             return 0.0;
        }

        let pnl_change = current_equity - self.last_equity_mark;
        
        // 更新水位线
        self.last_equity_mark = current_equity;
        
        pnl_change
    }
    
    pub fn calculate_quotes(&mut self, poly_mid_price: Decimal) -> (Decimal, Decimal) {
        // 1. 获取当前时间与剩余时间
        let now = chrono::Utc::now().timestamp_millis();
        let time_left_ms = self.cfg.maturity_timestamp_ms - now;
        
        // 如果市场已经结束，停止报价（或者报出一个极宽的价格）
        if time_left_ms <= 0 {
            return (dec!(0), dec!(0));
        }

        // 2. 将剩余时间标准化为“年” (AS 模型通常基于年化波动率)
        // 但在加密货币高频中，我们通常归一化为“天”或保留小时级敏感度
        // 这里我们用: T = 剩余天数。如果剩 1 小时，T = 0.04
        let t_days = time_left_ms as f64 / (1000.0 * 3600.0 * 24.0);

        // 3. 动态调整 Gamma (风险厌恶)
        // 逻辑：如果是“垃圾时间”(Closing Window)，我们极度厌恶持仓，Gamma 暴增
        let effective_gamma = if time_left_ms < (self.cfg.closing_window_seconds * 1000) {
            // 线性插值：时间越少，Gamma 越大，最大达到 terminal_dumping_factor 倍
            let progress = 1.0 - (time_left_ms as f64 / (self.cfg.closing_window_seconds * 1000.0 as f64));
            self.cfg.risk_aversion_gamma * (1.0 + progress * self.cfg.terminal_dumping_factor)
        } else {
            self.cfg.risk_aversion_gamma
        };

        // 4. 计算数据
        let sigma = self.vol_calc.update(poly_mid_price);
        let mid_f64 = poly_mid_price.to_f64().unwrap_or(0.5);
        
        // 5. AS 模型完全体公式
        // r = s - q * gamma * sigma^2 * T
        // 注意：这里的 T 实际上是一个“风险窗口”。
        // 在标准 AS 中，T 变小 Skew 变小。但在预测市场，如果你想平仓，必须配合上面的 effective_gamma 暴增。
        // 简单的工程实践：保留 T 项用于衰减长期风险，但在末端通过 Gamma 反向拉升。
        let risk_term = self.current_inventory_shares * effective_gamma * (sigma * sigma) * t_days.max(0.01); 
        let reservation_price = mid_f64 - risk_term;

        // 6. 动态价差 (Spread)
        // delta = gamma * sigma^2 * T + (2/gamma) * ln(1 + gamma/k)
        let spread_term_1 = effective_gamma * (sigma * sigma) * t_days.max(0.01);
        let spread_term_2 = (2.0 / effective_gamma) * (1.0 + effective_gamma / self.cfg.liquidity_k).ln();
        
        let half_spread = spread_term_1 + spread_term_2;

        // 7. 最小价差兜底 (防止 Gas 费亏损)
        let min_half = (self.cfg.min_spread_bps as f64 / 10000.0) / 2.0;
        let final_half_spread = half_spread.max(min_half);

        let raw_bid = reservation_price - final_half_spread;
        let raw_ask = reservation_price + final_half_spread;

        // 8. 边界检查 (0.01 - 0.99)
        (
            Self::round_to_tick(raw_bid, self.cfg.tick_size),
            Self::round_to_tick(raw_ask, self.cfg.tick_size)
        )
    }

    fn round_to_tick(price: f64, tick: f64) -> Decimal {
        let p = (price / tick).round() * tick;
        let p = p.max(0.01).min(0.99); // 预测市场价格边界
        Decimal::from_f64_retain(p).unwrap_or(dec!(0.5))
    }
}