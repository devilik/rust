// File: src/gateway/opinion_feed.rs

use crate::core::{InventoryUpdate, OrderBookUpdate, Exchange}; // [修改] 引入 OrderBookUpdate, Exchange
use crate::infrastructure::messaging::ZmqPublisher;
use futures_util::{StreamExt, SinkExt};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use serde_json::Value;
use std::str::FromStr;
use std::time::Duration;
use tokio::time;
use rust_decimal::Decimal; // [新增]
use rust_decimal_macros::dec; // [新增]
use smallvec::smallvec; // [新增]
use chrono::Utc; // [新增]

/// Opinion Labs WebSocket 监听器
/// 功能：
/// 1. 监听 trade.record.new -> 推送 InventoryUpdate (库存变化)
/// 2. 监听 market.last.price -> 推送 OrderBookUpdate (作为自定价锚点)
pub async fn run_opinion_ws_inventory_listener(
    zmq_pub: ZmqPublisher,
    ws_base_url: String, // e.g., "wss://ws.opinion.trade"
    market_id: String,
    api_key: String      // 用于鉴权
) {
    // 1. 构建鉴权 URL
    // 格式: wss://ws.opinion.trade?apikey={API_KEY}
    let url_string = format!("{}?apikey={}", ws_base_url, api_key);
    let url = Url::parse(&url_string).expect("Invalid Opinion WS URL");

    println!("👂 [OpinionFeed] Connecting to Opinion WS...");

    loop {
        match connect_async(url.clone()).await {
            Ok((ws_stream, _)) => {
                println!("✅ [OpinionFeed] Connected! Subscribing...");
                let (mut write, mut read) = ws_stream.split();

                // 2. 发送心跳包定时器 (每 30 秒)
                let mut heartbeat_interval = time::interval(Duration::from_secs(30));
                
                // 3. 发送订阅消息
                
                // [订阅 A] 库存变动 (Trade Executed)
                let sub_trade = serde_json::json!({
                    "action": "SUBSCRIBE",
                    "channel": "trade.record.new", 
                    "marketId": market_id.parse::<i64>().unwrap_or(0)
                });
                if let Err(e) = write.send(Message::Text(sub_trade.to_string())).await {
                     eprintln!("❌ [OpinionFeed] Trade Subscribe failed: {}", e);
                     continue; 
                }

                // [订阅 B] 最新成交价 (Market Last Price) -> 用于自定价模式
                let sub_price = serde_json::json!({
                    "action": "SUBSCRIBE",
                    "channel": "market.last.price",
                    "marketId": market_id.parse::<i64>().unwrap_or(0)
                });
                if let Err(e) = write.send(Message::Text(sub_price.to_string())).await {
                     eprintln!("❌ [OpinionFeed] Price Subscribe failed: {}", e);
                     continue; 
                }

                // 4. 事件循环
                loop {
                    tokio::select! {
                        _ = heartbeat_interval.tick() => {
                            let hb = serde_json::json!({"action": "HEARTBEAT"});
                            if let Err(_) = write.send(Message::Text(hb.to_string())).await {
                                break; // 发送失败重连
                            }
                        }
                        msg = read.next() => {
                            match msg {
                                Some(Ok(Message::Text(text))) => {
                                    handle_ws_message(&text, &zmq_pub, &market_id);
                                }
                                Some(Ok(Message::Ping(payload))) => {
                                    let _ = write.send(Message::Pong(payload)).await;
                                }
                                Some(Err(e)) => {
                                    eprintln!("❌ [OpinionFeed] WS Error: {}", e);
                                    break;
                                }
                                None => break, 
                                _ => {}
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("⚠️ [OpinionFeed] Connection failed: {}. Retrying in 5s...", e);
                time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

fn handle_ws_message(text: &str, zmq_pub: &ZmqPublisher, target_market_id_str: &str) {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };

    let msg_type = v["msgType"].as_str().unwrap_or("");
    let msg_market_id = v["marketId"].as_i64().unwrap_or(0).to_string();

    // 过滤非目标市场的消息
    if msg_market_id != target_market_id_str {
        return;
    }

    // --- Case 1: 库存更新 (Trade Executed) ---
    if msg_type == "trade.record.new" {
        if let (Some(shares_str), Some(side_str)) = (v["shares"].as_str(), v["side"].as_str()) {
            let shares = f64::from_str(shares_str).unwrap_or(0.0);
            let mut change = 0.0;

            match side_str {
                "Buy" => change = shares,
                "Sell" => change = -shares,
                _ => return, 
            }

            let cost_usd = v["usdAmount"].as_str()
                .and_then(|s| f64::from_str(s).ok())
                .unwrap_or(0.0);
            
            let net_cash_flow = if change > 0.0 { -cost_usd } else { cost_usd };

            println!("📦 [OpinionFeed WS] Trade Confirmed! Side: {} | Change: {:.2}", side_str, change);

            zmq_pub.send_inventory_update(&InventoryUpdate {
                symbol_id: msg_market_id.parse::<u64>().unwrap_or(0),
                change,
                cost_usd: net_cash_flow, 
            });
        }
    }
    // --- Case 2: [新增] 价格更新 (Market Last Price) ---
    else if msg_type == "market.last.price" {
        if let (Some(price_str), Some(outcome_side)) = (v["price"].as_str(), v["outcomeSide"].as_i64()) {
            // [关键] 只处理 OutcomeSide = 1 (Yes) 的价格
            // 如果 Opinion 市场是 Yes/No 结构，通常 Yes 价格是主要锚点
            if outcome_side == 1 {
                if let Ok(price) = Decimal::from_str(price_str) {
                    // 构造 OrderBookUpdate 推送给 Engine
                    // 这里我们构造一个"虚拟"的 Orderbook，Bid 和 Ask 都设为最新成交价
                    // 这样 Engine 在计算 Mid Price 时 ((Bid+Ask)/2) 就会得到这个成交价
                    let update = OrderBookUpdate {
                        exchange: Exchange::OpinionLabs, // 标记来源为 Opinion
                        symbol_id: msg_market_id.parse::<u64>().unwrap_or(0),
                        timestamp_ns: Utc::now().timestamp_nanos(),
                        bids: smallvec![(price, dec!(1000))], // 虚拟深度 1000
                        asks: smallvec![(price, dec!(1000))],
                    };
                    
                    zmq_pub.send_book_update(&update);
                    // 调试日志 (可选)
                    // println!("⚡ [OpinionFeed] Price Update: {}", price);
                }
            }
        }
    }
}