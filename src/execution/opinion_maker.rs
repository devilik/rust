use ethers::prelude::*;
use ethers::types::transaction::eip712::Eip712;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use rust_decimal::Decimal;
use crate::{TradeSignal, Side};
use std::time::Duration;

// 1. 定义一个中间结构体，承载签名后的数据
#[derive(Debug, Clone)]
pub struct SignedOrder {
    pub payload: serde_json::Value,
    pub order_id_tag: String, // 用于日志追踪
}

// 2. 订单结构体保持不变
#[derive(Debug, Clone, Eip712, EthAbiType, Serialize, Deserialize)]
#[eip712(
    name = "OpinionExchange",
    version = "1",
    chainId = 137,
    verifyingContract = "0x..." 
)]
#[derive(Debug, Clone)]
pub struct SignedOrder {
    pub payload: serde_json::Value,
    pub order_id_tag: String,
}

#[derive(Debug, Clone, Eip712, EthAbiType, Serialize, Deserialize)]
#[eip712(
    name = "OpinionExchange",
    version = "1",
    chainId = 137,
    verifyingContract = "0x..." 
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

pub struct OpinionMakerGateway {
    wallet: LocalWallet,
    http_client: reqwest::Client,
    api_url: String,
    api_key: String, // [新增] API Key 字段
}

impl OpinionMakerGateway {
    // [修改] 构造函数增加 api_key
    pub fn new(private_key: &str, api_url: &str, api_key: &str) -> Self {
        let wallet = private_key.parse::<LocalWallet>().unwrap()
            .with_chain_id(137u64);
        
        let client = reqwest::Client::builder()
            .tcp_nodelay(true)
            .pool_idle_per_host(100)
            .pool_max_idle_per_host(100)
            .timeout(Duration::from_secs(2))
            .build()
            .expect("Failed to create HTTP client");
            
        Self {
            wallet,
            http_client: client,
            api_url: api_url.to_string(),
            api_key: api_key.to_string(), // [新增]
        }
    }

    // ... (create_signed_order 保持不变) ...
    pub async fn create_signed_order(&self, signal: TradeSignal) -> Result<SignedOrder, Box<dyn std::error::Error + Send + Sync>> {
        let order_struct = LimitOrder {
            salt: rand::random::<u128>(),
            maker: self.wallet.address(),
            market_id: U256::from(signal.symbol_id),
            side: if signal.side == Side::Buy { 0 } else { 1 },
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

        Ok(SignedOrder {
            payload,
            order_id_tag: format!("{}-{}", signal.symbol_id, order_struct.salt),
        })
    }

    /// 阶段二：提交订单
    pub async fn submit_order(&self, signed_order: SignedOrder) -> Result<String, String> {
        let resp = self.http_client
            .post(format!("{}/order", self.api_url))
            .header("X-Opinion-Api-Key", &self.api_key) // [新增] 鉴权头
            .json(&signed_order.payload)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            Ok(signed_order.order_id_tag)
        } else {
            // 建议打印一下 Body 以便调试
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            Err(format!("HTTP {} - {}", status, text))
        }
    }

    /// 极速撤单
    pub async fn cancel_all(&self) -> Result<(), Box<dyn std::error::Error>> {
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis();
        let signature = self.wallet.sign_message(format!("CANCEL_ALL_{}", timestamp)).await?;

        self.http_client
            .delete(format!("{}/orders", self.api_url))
            .header("X-Opinion-Api-Key", &self.api_key) // [新增] 鉴权头
            .header("X-Signature", signature.to_string())
            .header("X-Timestamp", timestamp.to_string())
            .send()
            .await?;
            
        Ok(())
    }
}