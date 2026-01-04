use crate::infrastructure::messaging::ZmqPublisher;
use market_maker_core::{OrderBookUpdate, Exchange, Side};
use futures_util::{StreamExt, SinkExt};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use rust_decimal::Decimal;
use std::str::FromStr;
use smallvec::smallvec;

/// 启动监听器
pub async fn run_poly_feed_handler(zmq_pub: ZmqPublisher, market_ids: Vec<String>) {
    let url = Url::parse("wss://ws-poly.polymarket.com").expect("Invalid URL");

    println!("👂 [Gateway] Connecting to Polymarket WS...");
    
    // 1. 建立长连接 (Handshake)
    let (ws_stream, _) = connect_async(url).await.expect("Failed to connect");
    println!("✅ [Gateway] Connected!");

    let (mut write, mut read) = ws_stream.split();

    // 2. 发送订阅指令 (Subscription)
    // 这是告诉 Polymarket：“我要听这几个市场的声音”
    let sub_msg = serde_json::json!({
        "type": "Market",
        "assets_ids": market_ids, 
        "events": ["price_change", "order_book_update"] // 只要价格变动和订单簿更新
    });
    
    write.send(Message::Text(sub_msg.to_string())).await.expect("Subscribe failed");

    // 3. 死循环监听 (Event Loop)
    // 这里不是 Polling，是 Reactor 模式，有数据才会动
    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                // 收到 JSON 文本 -> 解析 -> 转换 -> 广播
                if let Some(update) = parse_poly_json(&text) {
                    // 🚀 这里的 send 就是把数据推入 ZMQ 管道
                    // 策略引擎那边就会收到数据
                    zmq_pub.send_book_update(&update);
                }
            }
            Ok(Message::Ping(payload)) => {
                // 自动回复 Pong，防止断连
                write.send(Message::Pong(payload)).await.unwrap_or(());
            }
            Err(e) => {
                println!("❌ WS Error: {:?}", e);
                break; // 真实环境这里需要写重连逻辑 (Reconnection)
            }
            _ => {}
        }
    }
}

/// 解析器：将 Polymarket 的脏 JSON 清洗为我们的干净结构体
fn parse_poly_json(raw: &str) -> Option<OrderBookUpdate> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;

    // 过滤掉无关消息
    if v["event_type"] != "order_book_update" {
        return None;
    }

    // 提取字段 (这里简化了错误处理)
    let timestamp = v["timestamp"].as_i64().unwrap_or(0);
    let asset_id_str = v["asset_id"].as_str()?;
    
    // 解析 Bids
    let mut bids = smallvec![];
    if let Some(arr) = v["bids"].as_array() {
        for quote in arr {
            let price = Decimal::from_str(quote["price"].as_str()?).ok()?;
            let size = Decimal::from_str(quote["size"].as_str()?).ok()?;
            bids.push((price, size));
        }
    }

    // 解析 Asks
    let mut asks = smallvec![];
    if let Some(arr) = v["asks"].as_array() {
        for quote in arr {
            let price = Decimal::from_str(quote["price"].as_str()?).ok()?;
            let size = Decimal::from_str(quote["size"].as_str()?).ok()?;
            asks.push((price, size));
        }
    }

    // 返回我们在 Module 1 定义的标准结构体
    Some(OrderBookUpdate {
        exchange: Exchange::Polymarket,
        symbol_id: u64::from_str_radix(&asset_id_str[2..], 16).unwrap_or(0), // 简单的 hash 模拟
        timestamp_ns: timestamp * 1_000_000, // ms -> ns
        bids,
        asks,
    })
}