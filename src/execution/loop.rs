use crate::infrastructure::messaging::ZmqSubscriber;
use crate::execution::opinion_maker::OpinionMakerGateway;
use crate::TradeSignal;
use std::sync::Arc;

pub async fn run_execution_loop() {
    // 监听 Engine 发来的信号
    let sub = ZmqSubscriber::new("tcp://localhost:5556", "SG");
    
    // 初始化 API Gateway (Maker 模式：只签名发请求，不耗 Gas)
    // 实际项目中请从 env 读取私钥
    let private_key = std::env::var("PRIVATE_KEY").unwrap_or("0x...".to_string());
    let gateway = Arc::new(OpinionMakerGateway::new(&private_key, "https://api.opinionlabs.xyz"));

    println!("🔫 [Execution] Ready to fire...");

    loop {
        // 1. 接收原始字节
        if let Some(msg_bytes) = sub.recv_raw_bytes() {
            // 2. 反序列化
            if let Ok(signal) = bincode::deserialize::<TradeSignal>(&msg_bytes) {
                // 3. 并发执行 (Fire-and-Forget)
                let gateway_clone = gateway.clone();
                
                tokio::spawn(async move {
                    // 这里的 place_order 已经修复了 decimals 问题
                    match gateway_clone.place_order(signal).await {
                        Ok(order_id) => println!("✅ Order Sent: {}", order_id),
                        Err(e) => eprintln!("❌ Order Error: {:?}", e),
                    }
                });
            }
        }
    }
}