use crate::core::InventoryUpdate;
use crate::infrastructure::messaging::ZmqPublisher;
use std::time::Duration;
use tokio::time;


/// 模拟/轮询 Opinion Labs 的仓位变化
/// 职责：只读 (Read-Only)
pub async fn run_opinion_chain_listener(
    zmq_pub: ZmqPublisher, 
    api_url_base: String, 
    market_id: String,
    api_key: String // 确保包含鉴权字段
) {
    // 1. 创建 HTTP Client
    let client = reqwest::Client::builder()
        .tcp_keepalive(Duration::from_secs(60))
        .pool_idle_per_host(10)
        .build()
        .unwrap();
        
    // 适配官方 API 路径
    let full_url = format!("{}/v1/positions", api_url_base); 
    let mut last_known_size = 0.0;
    
    println!("👂 [OpinionFeed] Polling positions for market: {}", market_id);

    loop {
        // 使用 API Key 发送请求
        let request = client.get(&full_url)
            .header("X-Opinion-Api-Key", &api_key)
            .query(&[("marketId", &market_id)]);

        match request.send().await {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    // 解析官方标准包装格式 { code: 0, data: ... }
                    if let Some(code) = json["code"].as_i64() {
                        if code == 0 {
                            if let Some(data) = json.get("result") {
                                // 根据实际返回结构解析
                                let current_size = data["size"].as_f64()
                                    .or_else(|| data["position"].as_f64()) 
                                    .unwrap_or(0.0);
                                
                                let change = current_size - last_known_size;
                                
                                // 只有显著变化才推送
                                if change.abs() > 1e-4 {
                                    println!("📦 [OpinionFeed] Detected Fill! {:.2} -> {:.2} (Delta: {:.2})", 
                                        last_known_size, current_size, change);
                                    
                                    zmq_pub.send_inventory_update(&InventoryUpdate {
                                        symbol_id: market_id.parse::<u64>().unwrap_or(0), 
                                        change: change,
                                        cost_usd: 0.0, 
                                    });
                                    
                                    last_known_size = current_size;
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("⚠️ [OpinionFeed] Poll failed: {}", e);
            }
        }

        // 避免过于频繁轮询
        time::sleep(Duration::from_millis(500)).await;
    }
}