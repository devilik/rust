use ethers::prelude::*;
use ethers::types::transaction::eip712::Eip712;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use rust_decimal::Decimal;
use crate::core::{TradeSignal, Side}; 

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