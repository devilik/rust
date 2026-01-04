use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use crate::math::volatility::RollingVolatility;

/// 策略参数配置 (热更新友好)
#[derive(Clone, Copy)]
pub struct StrategyConfig {
    pub risk_aversion_gamma: f64,  // γ: 风险厌恶 (越怕死，这个值越大)
    pub liquidity_k: f64,          // k: 市场流动性强度 (Opinion 刷量多，k 应该设大)
    pub min_spread_bps: u32,       // 最小保底价差 (防止亏 Gas)
    pub max_inventory_usd: f64,    // 最大持仓限制
    pub tick_size: f64,            // Opinion 的最小价格单位 (通常 0.01 或 0.0001)
}

pub struct OpinionGridStrategy {
    cfg: StrategyConfig,
    vol_calc: RollingVolatility,
    pub current_inventory_shares: f64, // 当前持仓 (正=Yes, 负=No)
}

impl OpinionGridStrategy {
    pub fn new(cfg: StrategyConfig) -> Self {
        Self {
            cfg,
            vol_calc: RollingVolatility::new(100), // 过去 100 个 Tick 的波动率
            current_inventory_shares: 0.0,
        }
    }

    /// 核心计算函数
    /// 输入: Polymarket 的公允价格
    /// 输出: (Bid Price, Ask Price)
    #[inline(always)]
    pub fn calculate_quotes(&mut self, poly_mid_price: Decimal) -> (Decimal, Decimal) {
        // 1. 更新波动率 (Sigma)
        let sigma = self.vol_calc.update(poly_mid_price);
        let mid_f64 = poly_mid_price.to_f64().unwrap_or(0.5);

        // --- Step A: 计算保留价格 (Reservation Price) ---
        // 公式: r = s - q * gamma * sigma^2
        // 特化: Opinion 价格波动剧烈，sigma^2 权重很大，一旦有波动，立即大幅偏离报价甩货
        let reservation_price = mid_f64 - (self.current_inventory_shares * self.cfg.risk_aversion_gamma * (sigma * sigma));

        // --- Step B: 计算最优半价差 (Optimal Half Spread) ---
        // 公式: delta = gamma * sigma^2 + (2/gamma) * ln(1 + gamma/k)
        let risk_component = self.cfg.risk_aversion_gamma * (sigma * sigma);
        let liquidity_component = (2.0 / self.cfg.risk_aversion_gamma) * (1.0 + self.cfg.risk_aversion_gamma / self.cfg.liquidity_k).ln();
        
        let mut half_spread = risk_component + liquidity_component;

        // --- Step C: 工程保护 (Guardrails) ---
        // 1. 最小价差保护 (覆盖 Taker Fee + Gas)
        let min_half_spread = (self.cfg.min_spread_bps as f64 / 10000.0) / 2.0;
        half_spread = half_spread.max(min_half_spread);

        // 2. 价格边界截断
        let raw_bid = reservation_price - half_spread;
        let raw_ask = reservation_price + half_spread;

        (
            Self::round_to_tick(raw_bid, self.cfg.tick_size),
            Self::round_to_tick(raw_ask, self.cfg.tick_size)
        )
    }

    /// 价格规整 (Tick Size Rounding)
    fn round_to_tick(price: f64, tick: f64) -> Decimal {
        let p = (price / tick).round() * tick;
        // 确保在 [0.01, 0.99] 之间
        Decimal::from_f64_retain(p.max(0.01).min(0.99)).unwrap_or(dec!(0.5))
    }
}