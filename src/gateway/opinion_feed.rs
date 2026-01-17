// File: src/gateway/opinion_feed.rs

use crate::core::InventoryUpdate;
use crate::infrastructure::messaging::ZmqPublisher;
use futures_util::{StreamExt, SinkExt};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use serde_json::Value;
use std::str::FromStr;
use std::time::Duration;
use tokio::time;

/// Opinion Labs WebSocket 库存监听器
/// 替代原本的 HTTP 轮询，实现毫秒级库存推送
pub async fn run_opinion_ws_inventory_listener(
    zmq_pub: ZmqPublisher,
    ws_base_url: String, // e.g., "wss://ws.opinion.trade"
    market_id: String,
    api_key: String      // 用于鉴权
) {
    // 1. 构建鉴权 URL [cite: 14]
    // 格式: wss://ws.opinion.trade?apikey={API_KEY}
    let url_string = format!("{}?apikey={}", ws_base_url, api_key);
    let url = Url::parse(&url_string).expect("Invalid Opinion WS URL");

    println!("👂 [OpinionFeed] Connecting to Opinion WS for Inventory Sync...");

    loop {
        match connect_async(url.clone()).await {
            Ok((ws_stream, _)) => {
                println!("✅ [OpinionFeed] Connected! Subscribing to trade updates...");
                let (mut write, mut read) = ws_stream.split();

                // 2. 发送心跳包任务 (每 30 秒) [cite: 14, 15]
                // "To maintain connection, send a HEARTBEAT message... every 30 seconds"
                let mut heartbeat_interval = time::interval(Duration::from_secs(30));
                let mut heartbeat_write = write; // Move ownership to separate task if needed, but here we keep simple
                
                // 由于 Rust ownership，这里我们在主循环里处理写，或者使用 select!
                // 为了简化，我们先发送订阅消息
                
                // 3. 订阅 "Trade Executed" 频道 
                // 该频道会在你的订单成交并上链后推送消息
                let sub_msg = serde_json::json!({
                    "action": "SUBSCRIBE",
                    "channel": "trade.record.new", 
                    "marketId": market_id.parse::<i64>().unwrap_or(0)
                });
                
                if let Err(e) = heartbeat_write.send(Message::Text(sub_msg.to_string())).await {
                    eprintln!("❌ [OpinionFeed] Subscribe failed: {}", e);
                    continue;
                }

                // 4. 事件循环 (Event Loop)
                loop {
                    tokio::select! {
                        _ = heartbeat_interval.tick() => {
                            // 发送心跳 [cite: 15]
                            let hb = serde_json::json!({"action": "HEARTBEAT"});
                            if let Err(_) = heartbeat_write.send(Message::Text(hb.to_string())).await {
                                break; // 发送失败，触发重连
                            }
                        }
                        msg = read.next() => {
                            match msg {
                                Some(Ok(Message::Text(text))) => {
                                    handle_ws_message(&text, &zmq_pub, &market_id);
                                }
                                Some(Ok(Message::Ping(payload))) => {
                                    let _ = heartbeat_write.send(Message::Pong(payload)).await;
                                }
                                Some(Err(e)) => {
                                    eprintln!("❌ [OpinionFeed] WS Error: {}", e);
                                    break;
                                }
                                None => break, // 连接关闭
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
    // 解析 JSON
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };

    // 过滤掉心跳回复等无关消息
    // 确认消息类型为 trade.record.new [cite: 23]
    if v["msgType"] != "trade.record.new" {
        return;
    }

    // 校验 Market ID (虽然我们只订阅了一个，但防御性编程)
    let msg_market_id = v["marketId"].as_i64().unwrap_or(0).to_string();
    if msg_market_id != target_market_id_str {
        return;
    }

    // 解析字段 [cite: 23, 25]
    // "shares": "amount of conditional token"
    // "side": "Buy" | "Sell"
    // "outcomeSide": 1 - yes, 2 - no (虽然做市通常只做一个方向，但需要注意)
    
    if let (Some(shares_str), Some(side_str)) = (v["shares"].as_str(), v["side"].as_str()) {
        let shares = f64::from_str(shares_str).unwrap_or(0.0);
        let mut change = 0.0;

        // 逻辑融合：将 WS 消息转换为库存 Delta
        // Buy: 获得 shares (库存增加)
        // Sell: 失去 shares (库存减少)
        match side_str {
            "Buy" => change = shares,
            "Sell" => change = -shares,
            _ => return, // Split/Merge 暂时忽略
        }

        // 解析成本 (用于计算 Realized PnL，可选)
        let cost_usd = v["usdAmount"].as_str()
            .and_then(|s| f64::from_str(s).ok())
            .unwrap_or(0.0);
        
        let net_cash_flow = if change > 0.0 { -cost_usd } else { cost_usd };

        println!("📦 [OpinionFeed WS] Trade Confirmed! Side: {} | Change: {:.2}", side_str, change);

        // 推送给 Engine
        zmq_pub.send_inventory_update(&InventoryUpdate {
            symbol_id: msg_market_id.parse::<u64>().unwrap_or(0),
            change,
            cost_usd: net_cash_flow, 
        });
    }
}