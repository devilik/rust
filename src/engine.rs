use crate::core::{OrderBookUpdate, TradeSignal, Exchange, Side, InventoryUpdate};
use crate::model::as_logic::{OpinionGridStrategy, StrategyConfig};
use crate::infrastructure::messaging::{ZmqSubscriber, ZmqPublisher};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;

pub fn run_strategy_engine() {
    let sub = ZmqSubscriber::new("tcp://localhost:5555", ""); 
    let pub_sock = ZmqPublisher::new("tcp://localhost:5556");

    // [配置] ID 映射表
    let mut id_map: HashMap<u64, u64> = HashMap::new();
    // ⚠️ 请填入真实数据: Polymarket Asset ID -> Opinion Market ID
    id_map.insert(217426331, 1); 

    let config = StrategyConfig {
        risk_aversion_gamma: 0.005,
        liquidity_k: 5000.0,
        min_spread_bps: 100,
        max_inventory_usd: 2000.0,
        tick_size: 0.01,
    };
    let mut strategy = OpinionGridStrategy::new(config);

    println!("🧠 [Engine] Active...");

    loop {
        let msg = match sub.recv_raw_bytes() {
            Some(m) => m,
            None => continue,
        };
        
        // 1. 尝试解析为行情数据
        if let Ok(update) = bincode::deserialize::<OrderBookUpdate>(&msg) {
             if update.exchange == Exchange::Polymarket {
                let opinion_market_id = match id_map.get(&update.symbol_id) {
                    Some(id) => *id,
                    None => continue,
                };

                let best_bid = update.bids.get(0).map(|x| x.0).unwrap_or(dec!(0));
                let best_ask = update.asks.get(0).map(|x| x.0).unwrap_or(dec!(0));
                if best_bid.is_zero() { continue; }
                let mid_price = (best_bid + best_ask) / dec!(2);

                let (new_bid, new_ask) = strategy.calculate_quotes(mid_price);
                let now = chrono::Utc::now().timestamp_nanos();

                // 发送信号
                pub_sock.send_signal(&TradeSignal {
                    strategy_id: 1,
                    target_exchange: Exchange::OpinionLabs,
                    symbol_id: opinion_market_id,
                    side: Side::Buy,
                    price: new_bid,
                    size_usd: dec!(50),
                    logic_tag: 1,
                    created_at_ns: now,
                });
                
                 pub_sock.send_signal(&TradeSignal {
                    strategy_id: 1,
                    target_exchange: Exchange::OpinionLabs,
                    symbol_id: opinion_market_id,
                    side: Side::Sell,
                    price: new_ask,
                    size_usd: dec!(50),
                    logic_tag: 1,
                    created_at_ns: now,
                });
            }
        } 
        // 2. 尝试解析为库存更新
        else if let Ok(inv_update) = bincode::deserialize::<InventoryUpdate>(&msg) {
            strategy.update_inventory(inv_update.change);
            println!("⚖️ Inventory Updated: {}", strategy.current_inventory_shares);
        }
    }
}