use crate::infrastructure::messaging::ZmqSubscriber;
use crate::execution::opinion_maker::OpinionMakerGateway;
use crate::core::TradeSignal;
use std::sync::Arc;

pub async fn run_execution_loop() {
    let sub = ZmqSubscriber::new("tcp://localhost:5556", "SG");
    
    // ⚠️ 从环境变量读取私钥
    let pk = std::env::var("PRIVATE_KEY").unwrap_or("0x...".to_string());
    let gateway = Arc::new(OpinionMakerGateway::new(&pk, "https://api.opinionlabs.xyz"));

    println!("🔫 [Execution] Ready...");

    loop {
        if let Some(msg) = sub.recv_raw_bytes() {
            if let Ok(signal) = bincode::deserialize::<TradeSignal>(&msg) {
                let gw = gateway.clone();
                tokio::spawn(async move {
                    match gw.place_order(signal).await {
                        Ok(id) => println!("✅ Sent: {}", id),
                        Err(e) => eprintln!("❌ Error: {:?}", e),
                    }
                });
            }
        }
    }
}