use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::VecDeque;

pub struct RollingVolatility {
    window_size: usize,
    returns: VecDeque<f64>, // 存 ln(Pt / Pt-1)
    last_price: Option<f64>,
    sum: f64,
    sum_sq: f64,
}

impl RollingVolatility {
    pub fn new(window_size: usize) -> Self {
        Self {
            window_size,
            returns: VecDeque::with_capacity(window_size),
            last_price: None,
            sum: 0.0,
            sum_sq: 0.0,
        }
    }

    pub fn update(&mut self, new_price_dec: Decimal) -> f64 {
        let new_price = new_price_dec.to_f64().unwrap_or(0.0);
        
        // 1. 计算对数收益率
        let ret = match self.last_price {
            Some(last) if last > 0.0 && new_price > 0.0 => (new_price / last).ln(),
            _ => 0.0,
        };
        self.last_price = Some(new_price);

        // 如果是第一个点，由于没有收益率，返回 0
        if ret == 0.0 && self.returns.is_empty() { return 0.0; }

        // 2. Welford 增量更新
        self.returns.push_back(ret);
        self.sum += ret;
        self.sum_sq += ret * ret;

        if self.returns.len() > self.window_size {
            if let Some(old) = self.returns.pop_front() {
                self.sum -= old;
                self.sum_sq -= old * old;
            }
        }

        // 3. 计算标准差
        let n = self.returns.len() as f64;
        if n < 2.0 { return 0.0; }

        let mean = self.sum / n;
        let variance = (self.sum_sq / n) - (mean * mean);
        
        variance.max(0.0).sqrt()
    }
}