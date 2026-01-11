use serde::Deserialize;
use crate::model::as_logic::StrategyConfig;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub system: SystemConfig,
    pub network: NetworkConfig,
    pub markets: MarketsConfig,
    pub strategy: StrategyConfig, // 直接复用你已有的结构体
    pub risk: RiskConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SystemConfig {
    pub env: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NetworkConfig {
    pub polymarket_ws_url: String,
    pub opinion_api_url: String,
    pub zmq_pub_endpoint: String,
    pub zmq_sub_endpoint: String,
    pub zmq_exec_endpoint: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MarketsConfig {
    pub polymarket_ids: Vec<String>,
    pub target_market_id: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RiskConfig {
    pub max_drawdown_usd: f64,
    pub max_order_size_usd: f64,
}

impl AppConfig {
    pub fn load(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: AppConfig = toml::from_str(&content)?;
        Ok(config)
    }
}