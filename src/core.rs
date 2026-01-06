use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Exchange {
    Polymarket = 1,
    OpinionLabs = 2,
    Unknown = 0,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

// 1. 行情数据快照 (来自 Polymarket Feed)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookUpdate {
    pub exchange: Exchange,
    pub symbol_id: u64, // Polymarket Asset ID (Hash)
    pub timestamp_ns: i64,
    pub bids: SmallVec<[(Decimal, Decimal); 10]>,
    pub asks: SmallVec<[(Decimal, Decimal); 10]>,
}

// 2. 库存更新事件 (来自 Opinion Feed)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryUpdate {
    pub symbol_id: u64, // Opinion Market ID
    pub change: f64,    // 仓位变化 (如 +10.0, -5.0)
}

// 3. 交易信号 (策略 -> 执行)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeSignal {
    pub strategy_id: u8,
    pub target_exchange: Exchange,
    pub symbol_id: u64, // Opinion Market ID
    pub side: Side,
    pub price: Decimal,
    pub size_usd: Decimal,
    pub logic_tag: u8,
    pub created_at_ns: i64,
}