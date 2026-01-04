use crate::{OrderBookUpdate, TradeSignal, Exchange, Side};
use crate::model::as_logic::{OpinionGridStrategy, StrategyConfig};
use crate::infrastructure::messaging::{ZmqSubscriber, ZmqPublisher};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub fn run_strategy_engine() {
    // 1. 订阅行情 (来自 Feed), 发布信号 (给 Execution)
    let sub = ZmqSubscriber::new("tcp://localhost:5555", "MD");
    let pub_sock = ZmqPublisher::new("tcp://localhost:5556");

    // 2. 初始化 AS 策略
    let config = StrategyConfig {
        risk_aversion_gamma: 0.005,
        liquidity_k: 5000.0,
        min_spread_bps: 100, // 1%
        max_inventory_usd: 2000.0,
        tick_size: 0.01,
    };
    let mut strategy = OpinionGridStrategy::new(config);

    println!("🧠 [Engine] Strategy Active & Listening...");

    loop {
        // A. 阻塞接收 (Zero Copy)
        let update = match sub.recv_book_update() {
            Some(u) => u,
            None => continue,
        };

        match update.exchange {
            // 只处理 Polymarket 数据作为定价锚点
            Exchange::Polymarket => {
                let best_bid = update.bids.get(0).map(|x| x.0).unwrap_or(dec!(0));
                let best_ask = update.asks.get(0).map(|x| x.0).unwrap_or(dec!(0));
                if best_bid.is_zero() || best_ask.is_zero() { continue; }
                
                let mid_price = (best_bid + best_ask) / dec!(2);

                // B. 核心计算
                let (new_bid, new_ask) = strategy.calculate_quotes(mid_price);

                // C. 生成信号
                let timestamp = chrono::Utc::now().timestamp_nanos();
                
                let bid_sig = TradeSignal {
                    strategy_id: 1,
                    target_exchange: Exchange::OpinionLabs,
                    symbol_id: update.symbol_id, // 对应 Opinion 的 Market ID
                    side: Side::Buy,
                    price: new_bid,
                    size_usd: dec!(50),
                    logic_tag: 1,
                    created_at_ns: timestamp,
                };
                
                let ask_sig = TradeSignal {
                    strategy_id: 1,
                    target_exchange: Exchange::OpinionLabs,
                    symbol_id: update.symbol_id,
                    side: Side::Sell,
                    price: new_ask,
                    size_usd: dec!(50),
                    logic_tag: 1,
                    created_at_ns: timestamp,
                };

                // D. 发送信号 (现在 send_signal 已经存在了)
                pub_sock.send_signal(&bid_sig);
                pub_sock.send_signal(&ask_sig);
            }
            _ => {}
        }
    }
}