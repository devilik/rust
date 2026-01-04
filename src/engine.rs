use crate::{OrderBookUpdate, TradeSignal, Exchange, Side};
use crate::model::as_logic::{OpinionGridStrategy, StrategyConfig};
use crate::infrastructure::messaging::{ZmqSubscriber, ZmqPublisher};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// 策略引擎主进程
pub fn run_strategy_engine() {
    // 1. 初始化通信 (ZMQ)
    let sub = ZmqSubscriber::new("tcp://localhost:5555", "MD"); // 订阅行情
    let pub_sock = ZmqPublisher::new("tcp://localhost:5556");   // 发布信号

    // 2. 初始化策略状态 (Opinion 特化配置)
    let config = StrategyConfig {
        risk_aversion_gamma: 0.005, // 稍微激进一点，因为我们要快速周转库存
        liquidity_k: 5000.0,        // 假设每天成交量很大 (刷分党贡献)
        min_spread_bps: 100,        // 最小 1% Spread (Opinion 手续费高，不能太窄)
        max_inventory_usd: 2000.0,
        tick_size: 0.01,
    };
    let mut strategy = OpinionGridStrategy::new(config);

    println!("🚀 Strategy Engine Started: Pinning to Core...");

    // 3. 极速主循环 (Hot Loop)
    loop {
        // A. 非阻塞读取行情 (Zero Copy)
        // 如果没有新消息，立即 continue，不要 sleep，保持 CPU 100% 运转以减少唤醒延迟
        let update = match sub.recv_book_update() {
            Some(u) => u,
            None => continue, 
        };

        // B. 处理逻辑
        match update.exchange {
            // 情况 1: Polymarket 数据来了 -> 更新锚定价格 & 重新计算 Quote
            Exchange::Polymarket => {
                // 取中间价
                let best_bid = update.bids.get(0).map(|x| x.0).unwrap_or(dec!(0));
                let best_ask = update.asks.get(0).map(|x| x.0).unwrap_or(dec!(1));
                if best_bid.is_zero() { continue; }
                let mid_price = (best_bid + best_ask) / dec!(2);

                // --- 核心计算 (200ns) ---
                let (new_bid, new_ask) = strategy.calculate_quotes(mid_price);

                // --- 生成信号 (Place Order) ---
                // 我们生成 "Diff" 信号：实际执行层会判断价格变动是否超过阈值，避免频繁改单
                let bid_sig = TradeSignal {
                    target_exchange: Exchange::OpinionLabs,
                    side: Side::Buy,
                    price: new_bid,
                    size_usd: dec!(50), // 单笔 50U
                    logic_tag: 1,       // 1 = Market Make
                    created_at_ns: chrono::Utc::now().timestamp_nanos(),
                    ..Default::default()
                };
                
                let ask_sig = TradeSignal {
                    target_exchange: Exchange::OpinionLabs,
                    side: Side::Sell,
                    price: new_ask,
                    size_usd: dec!(50),
                    logic_tag: 1,
                    created_at_ns: chrono::Utc::now().timestamp_nanos(),
                    ..Default::default()
                };

                // C. 极速发布
                pub_sock.send_signal(&bid_sig);
                pub_sock.send_signal(&ask_sig);
            }

            // 情况 2: Opinion Labs 自己的成交数据 -> 更新库存
            Exchange::OpinionLabs => {
                // 这里需要解析 Trade 事件，更新 strategy.current_inventory_shares
                // 暂时略过，这部分逻辑通常由 OrderManager 回传
            }
            
            _ => {}
        }
    }
}