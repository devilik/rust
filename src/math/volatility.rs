use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::collections::VecDeque;

/// 高性能在线波动率计算器
/// 维护一个固定窗口，新数据进，旧数据出，O(1) 更新标准差
pub struct RollingVolatility {
    window_size: usize,
    prices: VecDeque<f64>, // 使用 f64 进行中间计算以利用 FPU/SIMD 加速
    sum: f64,
    sum_sq: f64,
}

impl RollingVolatility {
    pub fn new(window_size: usize) -> Self {
        Self {
            window_size,
            prices: VecDeque::with_capacity(window_size), // 预分配内存
            sum: 0.0,
            sum_sq: 0.0,
        }
    }

    /// 核心优化：O(1) 时间复杂度更新，无循环
    #[inline(always)] 
    pub fn update(&mut self, new_price_dec: Decimal) -> f64 {
        let new_price = new_price_dec.to_f64().unwrap_or(0.0);
        
        // 1. 推入新数据
        self.prices.push_back(new_price);
        self.sum += new_price;
        self.sum_sq += new_price * new_price;

        // 2. 移除旧数据 (如果满了)
        if self.prices.len() > self.window_size {
            if let Some(old_price) = self.prices.pop_front() {
                self.sum -= old_price;
                self.sum_sq -= old_price * old_price;
            }
        }

        // 3. 计算标准差 (Sigma)
        let n = self.prices.len() as f64;
        if n < 2.0 {
            return 0.0;
        }

        // Variance = E[X^2] - (E[X])^2
        let mean = self.sum / n;
        let variance = (self.sum_sq / n) - (mean * mean);
        
        // 返回波动率 (防止浮点误差导致负数)
        variance.max(0.0).sqrt()
    }
}