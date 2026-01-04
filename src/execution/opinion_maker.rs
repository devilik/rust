use ethers::prelude::*;
use ethers::types::transaction::eip712::Eip712;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use rust_decimal::Decimal;
use crate::{TradeSignal, Side};

// --- A. 定义 Opinion Labs 的订单结构 (EIP-712) ---
// 这必须与 Opinion Labs 智能合约/官方文档中的结构完全一致，否则签名会失败
#[derive(Debug, Clone, Eip712, EthAbiType, Serialize, Deserialize)]
#[eip712(
    name = "OpinionExchange", // 域名，需查阅官方文档
    version = "1",
    chainId = 137,            // Polygon Chain ID
    verifyingContract = "0x..." // Opinion Labs 的核心合约地址
)]
pub struct LimitOrder {
    pub salt: u128,          // 随机数，防止重放攻击
    pub maker: Address,      // 你的钱包地址
    pub market_id: U256,     // 市场 ID
    pub side: u8,            // 0 = Buy, 1 = Sell
    pub price: U256,         // 价格 (通常放大了 1e18 或 1e6 倍)
    pub size: U256,          // 数量
    pub expiration: u64,     // 订单过期时间 (做市商通常设为 1分钟后过期，防止僵尸单)
}

// --- B. 执行网关 ---
pub struct OpinionMakerGateway {
    wallet: LocalWallet,       // 用于签名的钱包
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
        // 1. 构建 EIP-712 订单对象
        let order_struct = LimitOrder {
            salt: rand::random::<u128>(),
            maker: self.wallet.address(),
            market_id: U256::from(signal.symbol_id),
            side: if signal.side == Side::Buy { 0 } else { 1 },
            // 注意精度转换：假设 Opinion 使用 18 位精度
            price: ethers::utils::parse_ether(signal.price)?, 
            size: ethers::utils::parse_ether(signal.size_usd)?,
            expiration: 0, // 0 通常代表 Good-Till-Cancel (GTC)
        };

        // 2. 链下签名 (Off-Chain Signing) - 这一步极快，不需要网络
        let signature = self.wallet.sign_typed_data(&order_struct).await?;

        // 3. 构建 API Payload
        let payload = serde_json::json!({
            "order": order_struct,
            "signature": signature.to_string(),
            "strategy_tag": "RUST_MM_BOT" // 给自己打个标签，方便后台查单
        });

        // 4. 发送给 Opinion Labs 服务器 (API 竞速)
        // 这里是整个链路中耗时最长的一步 (网络 IO)
        let resp = self.http_client
            .post(format!("{}/order", self.api_url))
            .json(&payload)
            .send()
            .await?;

        if resp.status().is_success() {
            // 返回订单 ID
            let resp_json: serde_json::Value = resp.json().await?;
            Ok(resp_json["orderId"].as_str().unwrap_or("").to_string())
        } else {
            Err(format!("API Error: {:?}", resp.text().await?).into())
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