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
}

impl OpinionMakerGateway {
    pub fn new(private_key: &str, api_url: &str) -> Self {
        let wallet = private_key.parse::<LocalWallet>().unwrap()
            .with_chain_id(137u64);
        
        // [优化点 1] 激进的 HTTP 连接池配置
        let client = reqwest::Client::builder()
            .tcp_nodelay(true)           // 禁用 Nagle 算法，有数据立即发送
            .pool_idle_per_host(100)     // 保持更多空闲连接
            .pool_max_idle_per_host(100)
            .timeout(Duration::from_secs(2)) // 2秒超时，HFT 不需要等太久
            .build()
            .expect("Failed to create HTTP client");
            
        Self {
            wallet,
            http_client: client,
            api_url: api_url.to_string(),
        }
    }

    /// 阶段一：纯 CPU 计算 (签名)
    /// 这个函数执行非常快，不涉及网络 IO
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

        // 签名 (CPU 密集)
        let signature = self.wallet.sign_typed_data(&order_struct).await?;

        // 构建 Payload
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

    /// 阶段二：纯网络 IO (发送)
    /// 这里的耗时是不确定的 (50ms - 500ms)
    pub async fn submit_order(&self, signed_order: SignedOrder) -> Result<String, String> {
        let resp = self.http_client
            .post(format!("{}/order", self.api_url))
            .json(&signed_order.payload)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            // 这里为了追求极致速度，甚至可以不解析 Body，直接返回 OK
            Ok(signed_order.order_id_tag)
        } else {
            Err(format!("HTTP {}", resp.status()))
        }
    }

    /// 极速撤单 (Batch Cancel)
    /// 做市商最关键的功能：一键撤回所有报价
    pub async fn cancel_all(&self) -> Result<(), Box<dyn std::error::Error>> {
        // 撤单通常也需要 EIP-712 签名
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis();
        
        // 假设撤单只需要签一个时间戳
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