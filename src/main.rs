// File: src/main.rs

mod infrastructure;
mod model;
mod math;
mod gateway;
mod engine;
mod execution;
mod core;
mod config; 

use infrastructure::messaging::ZmqPublisher;
use gateway::poly_feed::run_poly_feed_handler;
use gateway::opinion_feed::run_opinion_chain_listener;
use engine::run_strategy_engine;
use execution::event_loop::run_execution_loop;
use config::AppConfig; 
use std::process;

#[tokio::main]
async fn main() {
    println!("🚀 Starting Enterprise Market Maker System...");

    // 1. 加载配置文件
    let config = match AppConfig::load("config.toml") {
        Ok(c) => {
            println!("✅ Configuration loaded successfully.");
            c
        },
        Err(e) => {
            eprintln!("❌ Failed to load config.toml: {}", e);
            process::exit(1);
        }
    };

    // 初始化 ZMQ Publisher (用于广播行情和库存更新)
    // 这里的 zmq_pub_endpoint 通常是 "tcp://*:5555" (Bind)
    let market_data_pub = ZmqPublisher::new(&config.network.zmq_pub_endpoint);

    // 2. 启动 Polymarket 数据源 (WebSocket 监听)
    let poly_pub = market_data_pub.clone();
    let poly_config = config.clone(); 
    tokio::spawn(async move {
        let markets = poly_config.markets.polymarket_ids; 
        let url = poly_config.network.polymarket_ws_url;
        
        println!("👂 [PolyFeed] Starting listener for {} markets...", markets.len());
        run_poly_feed_handler(poly_pub, url, markets).await;
    });

    // 3. 启动 Opinion Labs 库存监听 (API 轮询)
    // [修改点] 传入 API URL 和 目标市场 ID，用于获取实时持仓
    let opinion_pub = market_data_pub.clone();
    
    // 注意：文档指出 WS 地址通常是 wss://ws.opinion.trade
    // 我们可以从 config 读取，或者硬编码 Base URL
    let ws_url = "wss://ws.opinion.trade".to_string(); 
    let target_market_id = config.markets.target_market_id.to_string();
    let api_key = config.network.api_key.clone(); // 确保 Config 中有这个字段

    tokio::spawn(async move {
        println!("👂 [OpinionFeed] Starting WS listener for Market ID: {}...", target_market_id);
        run_opinion_ws_inventory_listener(
            opinion_pub, 
            ws_url, 
            target_market_id,
            api_key
        ).await;
    });

    // 4. 启动执行引擎 (下单/撤单流水线)
    let exec_config = config.clone();
    tokio::spawn(async move {
        println!("🔫 [Execution] Starting execution loop...");
        run_execution_loop(
            exec_config.network.opinion_api_url,
            exec_config.network.zmq_exec_endpoint, 
            exec_config.network.api_key
        ).await;
    });

    // 5. 启动策略引擎 (核心大脑)
    // 策略引擎包含大量计算，且内部有 loop，建议用 spawn_blocking 或者单独的 thread
    // 但因为 run_strategy_engine 内部设计了 channel 且不是纯粹的 CPU 密集型(有 IO等待)，
    // 用 spawn_blocking 主要是为了防止它阻塞 tokio 的调度器。
    let strategy_config = config.clone();
    println!("🧠 [Strategy] Engine booting up...");
    
    let strategy_handle = tokio::task::spawn_blocking(move || {
        run_strategy_engine(strategy_config);
    });

    // 6. 守护进程退出
    // 等待策略引擎结束（通常策略引擎是死循环，除非收到 Ctrl+C 或熔断）
    match strategy_handle.await {
        Ok(_) => println!("✅ [Main] Strategy Engine exited gracefully."),
        Err(e) => eprintln!("❌ [Main] Strategy Engine crashed or panicked: {:?}", e),
    }
}