use ethers::prelude::*;
use ethers::types::transaction::eip712::Eip712;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use rust_decimal::Decimal;
use crate::core::{TradeSignal, Side}; 
use crate::infrastructure::messaging::ZmqPublisher;
use crate::core::InventoryUpdate;
use std::time::Duration;
use tokio::time;
// --- A. 定义 Opinion Labs 的订单结构 (EIP-712) ---
#[derive(Debug, Clone, Eip712, EthAbiType, Serialize, Deserialize)]
#[eip712(
    name = "OpinionExchange",
    version = "1",
    chainId = 137,
    verifyingContract = "0x..." // ⚠️ 务必替换为真实合约地址
)]
pub struct LimitOrder {
    pub salt: u128,
    pub maker: Address,
    pub market_id: U256,
    pub side: u8,
    pub price: U256,
    pub size: U256,
    pub expiration: u64,
}

// --- B. 执行网关 ---
pub struct OpinionMakerGateway {
    wallet: LocalWallet,
    http_client: reqwest::Client,
    api_url: String,
}

impl OpinionMakerGateway {
    pub fn new(private_key: &str, api_url: &str) -> Self {
        let wallet = private_key.parse::<LocalWallet>().unwrap()
            .with_chain_id(137u64);
            
        Self {
            wallet,
            http_client: reqwest::Client::new(),
            api_url: api_url.to_string(),
        }
    }

    /// 核心方法：将策略信号转化为 EIP-712 签名并发送
    pub async fn place_order(&self, signal: TradeSignal) -> Result<String, Box<dyn std::error::Error>> {
        let order_struct = LimitOrder {
            salt: rand::random::<u128>(),
            maker: self.wallet.address(),
            market_id: U256::from(signal.symbol_id),
            side: if signal.side == Side::Buy { 0 } else { 1 },
            // [关键修复] 使用 6 位精度 (USDC)
            price: ethers::utils::parse_units(signal.price, 6)?.into(), 
            size: ethers::utils::parse_units(signal.size_usd, 6)?.into(),
            expiration: 0, 
        };

        let signature = self.wallet.sign_typed_data(&order_struct).await?;

        let payload = serde_json::json!({
            "order": order_struct,
            "signature": signature.to_string(),
            "strategy_tag": "RUST_MM_BOT"
        });

        let resp = self.http_client
            .post(format!("{}/order", self.api_url))
            .json(&payload)
            .send()
            .await?;

        if resp.status().is_success() {
            let resp_json: serde_json::Value = resp.json().await?;
            Ok(resp_json["orderId"].as_str().unwrap_or("").to_string())
        } else {
            Err(format!("API Error: {:?}", resp.text().await?).into())
        }
    }
    
    /// 极速撤单 (Batch Cancel)
    /// 做市商保命键：一键撤回所有报价
    pub async fn cancel_all(&self) -> Result<(), Box<dyn std::error::Error>> {
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis();
        
        // 签名消息格式需参考官方文档，这里假设为 "CANCEL_ALL_{ts}"
        let signature = self.wallet.sign_message(format!("CANCEL_ALL_{}", timestamp)).await?;

        self.http_client
            .delete(format!("{}/orders", self.api_url))
            .header("X-Signature", signature.to_string())
            .header("X-Timestamp", timestamp.to_string())
            .send()
            .await?;
            
        Ok(())
    }
}
/// [新增] 模拟/轮询 Opinion Labs 的仓位变化
pub async fn run_opinion_chain_listener(zmq_pub: ZmqPublisher, api_url_base: String, market_id: String) {
    // 1. 创建 HTTP Client (复用连接池)
    let client = reqwest::Client::builder()
        .tcp_keepalive(Duration::from_secs(60))
        .pool_idle_per_host(10)
        .build()
        .unwrap();
        
    let full_url = format!("{}/v1/positions", api_url_base); // 假设的 API 路径
    let mut last_known_size = 0.0;
    
    // 简单的去重机制，防止重复发送同一笔成交
    // 真实场景最好用 OrderId 或 EventLogIndex
    
    println!("👂 [OpinionFeed] Polling positions for market: {}", market_id);

    loop {
        // 假设 API 返回格式: { "size": 10.0, "avgPrice": 0.5 }
        // 你需要根据真实的 Opinion Labs API 文档调整这里的解析逻辑
        match client.get(&full_url).query(&[("marketId", &market_id)]).send().await {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let current_size = json["size"].as_f64().unwrap_or(0.0);
                    
                    // 检测到变化 (增量)
                    let change = current_size - last_known_size;
                    
                    // 只有当变化显著时才推送 (处理浮点数精度)
                    if change.abs() > 1e-4 {
                        println!("📦 [OpinionFeed] Detected Fill! {:.2} -> {:.2} (Delta: {:.2})", 
                            last_known_size, current_size, change);
                        
                        // 推送增量更新给引擎
                        // 注意：cost_usd 这里暂时填 0，如果有 avgPrice 可以算出来
                        zmq_pub.send_inventory_update(&InventoryUpdate {
                            symbol_id: market_id.parse::<u64>().unwrap_or(0), 
                            change: change,
                            cost_usd: 0.0, 
                        });
                        
                        last_known_size = current_size;
                    }
                }
            }
            Err(e) => {
                // 网络错误不 panic，只是打印日志
                eprintln!("⚠️ [OpinionFeed] Poll failed: {}", e);
            }
        }

        // 200ms 轮询一次
        time::sleep(Duration::from_millis(200)).await;
    }
}