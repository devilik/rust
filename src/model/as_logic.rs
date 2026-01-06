use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use crate::math::volatility::RollingVolatility;

#[derive(Clone, Copy)]
pub struct StrategyConfig {
    pub risk_aversion_gamma: f64,
    pub liquidity_k: f64,
    pub min_spread_bps: u32,
    pub max_inventory_usd: f64,
    pub tick_size: f64,
}

pub struct OpinionGridStrategy {
    cfg: StrategyConfig,
    vol_calc: RollingVolatility,
    pub current_inventory_shares: f64, 
}

impl OpinionGridStrategy {
    pub fn new(cfg: StrategyConfig) -> Self {
        Self {
            cfg,
            vol_calc: RollingVolatility::new(100),
            current_inventory_shares: 0.0,
        }
    }

    pub fn update_inventory(&mut self, change: f64) {
        self.current_inventory_shares += change;
    }

    pub fn calculate_quotes(&mut self, poly_mid_price: Decimal) -> (Decimal, Decimal) {
        let sigma = self.vol_calc.update(poly_mid_price);
        let mid_f64 = poly_mid_price.try_into().unwrap_or(0.5);

        // 核心 AS 公式
        let reservation_price = mid_f64 - (self.current_inventory_shares * self.cfg.risk_aversion_gamma * (sigma * sigma));
        
        let half_spread = (self.cfg.risk_aversion_gamma * sigma * sigma) + 
                          ((2.0 / self.cfg.risk_aversion_gamma) * (1.0 + self.cfg.risk_aversion_gamma / self.cfg.liquidity_k).ln());
        
        let min_half = (self.cfg.min_spread_bps as f64 / 10000.0) / 2.0;
        let final_half_spread = half_spread.max(min_half);

        let raw_bid = reservation_price - final_half_spread;
        let raw_ask = reservation_price + final_half_spread;

        (
            Self::round_to_tick(raw_bid, self.cfg.tick_size),
            Self::round_to_tick(raw_ask, self.cfg.tick_size)
        )
    }

    fn round_to_tick(price: f64, tick: f64) -> Decimal {
        let p = (price / tick).round() * tick;
        Decimal::from_f64_retain(p.max(0.01).min(0.99)).unwrap_or(dec!(0.5))
    }
}