use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

/// 1. 交易所枚举 (支持多市场套利)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Exchange {
    Polymarket = 1,
    OpinionLabs = 2,
    Unknown = 0,
}

/// 2. 订单簿快照 (L2 Data)
/// 使用 SmallVec 优化：90%的情况下，更新的层级不超过10档，避免堆内存分配
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookUpdate {
    pub exchange: Exchange,
    pub symbol_id: u64,  // 用 u64 哈希替代 String 节省内存
    pub timestamp_ns: i64, // 纳秒级时间戳
    pub bids: SmallVec<[(Decimal, Decimal); 10]>, // [(price, size), ...]
    pub asks: SmallVec<[(Decimal, Decimal); 10]>,
}

/// 3. 策略信号 (Internal Signal)
/// 策略引擎发给执行引擎的指令
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeSignal {
    pub strategy_id: u8,   // 哪个策略发出的？
    pub target_exchange: Exchange,
    pub symbol_id: u64,
    pub side: Side,
    pub price: Decimal,
    pub size_usd: Decimal,
    pub logic_tag: u8,     // 例如 1=AS_SKEW, 2=ARBITRAGE
    pub created_at_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

/// 4. 共享内存环形缓冲区 (Ring Buffer) 接口定义
/// 這是真正 High Frequency 的关键：无锁队列
pub trait EventBus {
    fn publish_market_data(&self, data: &OrderBookUpdate);
    fn subscribe_market_data(&self) -> OrderBookUpdate;
    fn publish_signal(&self, signal: &TradeSignal);
}