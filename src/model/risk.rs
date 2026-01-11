// File: src/model/risk.rs
use crate::core::{Side, TradeSignal};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub struct RiskManager {
    // --- 硬参数 (Hard Limits) ---
    pub max_drawdown_usd: f64,    // 最大回撤阈值 (如 100 U)
    pub max_order_size_usd: f64,  // 单笔最大金额 (肥手指保护)
    pub stop_loss_price_floor: Decimal, // 价格下限保护
    pub stop_loss_price_ceiling: Decimal, // 价格上限保护

    // --- 运行时状态 ---
    pub total_pnl: f64,          // 累计盈亏
    pub peak_equity_pnl: f64,    // 历史最高盈亏水位 (用于计算回撤)
    pub current_drawdown: f64,   // 当前回撤值
    pub is_kill_switch_active: bool, // 是否熔断
}

impl RiskManager {
    pub fn new(cfg: crate::config::RiskConfig) -> Self {
        Self {
            max_drawdown_usd: max_drawdown,
            max_order_size_usd: max_order,
            stop_loss_price_floor: Decimal::try_from(cfg.price_floor).unwrap_or(dec!(0.02)), // 使用配置
            stop_loss_price_ceiling: Decimal::try_from(cfg.price_ceiling).unwrap_or(dec!(0.98)), // 使用配置
            
            total_pnl: 0.0,
            peak_equity_pnl: 0.0,
            current_drawdown: 0.0,
            is_kill_switch_active: false,
        }
    }

    /// [检查 1] 信号合规性检查 (Pre-Trade Check)
    /// 如果返回 false，Engine 必须丢弃该信号
    pub fn check_signal(&self, signal: &TradeSignal) -> bool {
        // 1. 熔断状态检查
        if self.is_kill_switch_active {
            // 只有撤单信号(逻辑一般不在这里处理)或者特殊平仓单可以通过
            // 但为了安全，熔断后拒绝一切新开仓
            return false; 
        }

        // 2. 肥手指检查
        let size_f64 = signal.size_usd.try_into().unwrap_or(0.0);
        if size_f64 > self.max_order_size_usd {
            eprintln!("🛡️ [RISK REJECT] Order size ${:.2} > Max ${:.2}", size_f64, self.max_order_size_usd);
            return false;
        }

        // 3. 价格异常检查 (防止预言机攻击或数据错误导致报出离谱价格)
        if signal.side == Side::Buy && signal.price > self.stop_loss_price_ceiling {
            eprintln!("🛡️ [RISK REJECT] Buying above ceiling: {}", signal.price);
            return false;
        }
        if signal.side == Side::Sell && signal.price < self.stop_loss_price_floor {
            eprintln!("🛡️ [RISK REJECT] Selling below floor: {}", signal.price);
            return false;
        }

        true
    }

    /// [检查 2] PnL 更新与熔断判定 (Post-Tick Check)
    /// 输入：pnl_change (这一瞬间的权益变化)
    /// 返回：true 表示刚刚触发了熔断，需要立即报警
    pub fn update_pnl_and_check_kill(&mut self, pnl_change: f64) -> bool {
        if self.is_kill_switch_active {
            return false; // 已经死了，不再触发
        }

        self.total_pnl += pnl_change;

        // 高水位法 (High-Water Mark) 计算回撤
        if self.total_pnl > self.peak_equity_pnl {
            self.peak_equity_pnl = self.total_pnl;
            self.current_drawdown = 0.0;
        } else {
            // 回撤 = 最高点 - 当前点
            self.current_drawdown = self.peak_equity_pnl - self.total_pnl;
        }

        // 检查阈值
        if self.current_drawdown > self.max_drawdown_usd {
            self.is_kill_switch_active = true;
            eprintln!("\n🚨🚨🚨 [KILL SWITCH TRIGGERED] 🚨🚨🚨");
            eprintln!("Reason: Max Drawdown Exceeded");
            eprintln!("Current Drawdown: ${:.4} (Limit: ${:.2})", self.current_drawdown, self.max_drawdown_usd);
            eprintln!("Total PnL: ${:.4}", self.total_pnl);
            return true;
        }

        false
    }
}