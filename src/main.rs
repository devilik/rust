mod infrastructure;
mod model;
mod math;
mod gateway;
mod engine;
mod execution;
mod core;
mod config; // 注册新模块

use infrastructure::messaging::ZmqPublisher;
use gateway::poly_feed::run_poly_feed_handler;
use gateway::opinion_feed::run_opinion_chain_listener;
use engine::run_strategy_engine;
use execution::event_loop::run_execution_loop;
use config::AppConfig; // 引入配置结构体
use std::process;

#[tokio::main]
async fn main() {
    println!("🚀 Starting Enterprise Market Maker System...");

    // 1. [新增] 加载配置文件
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

    // 初始化 ZMQ Publisher (使用配置中的端口)
    let market_data_pub = ZmqPublisher::new(&config.network.zmq_pub_endpoint);

    // 2. 启动 Polymarket 数据源
    let poly_pub = market_data_pub.clone();
    let poly_config = config.clone(); // 克隆配置供 Task 使用
    tokio::spawn(async move {
        // [修改] 从配置读取
        let markets = poly_config.markets.polymarket_ids; 
        let url = poly_config.network.polymarket_ws_url;
        
        println!("👂 [PolyFeed] Starting listener for {} markets...", markets.len());
        run_poly_feed_handler(poly_pub, url, markets).await;
    });

    // 3. 启动 Opinion 链上监听
    let opinion_pub = market_data_pub.clone();
    tokio::spawn(async move {
        println!("👂 [OpinionFeed] Starting chain listener...");
        run_opinion_chain_listener(opinion_pub).await;
    });

    // 4. 启动执行引擎
    let exec_config = config.clone();
    tokio::spawn(async move {
        println!("🔫 [Execution] Starting execution loop...");
        // [修改] 传入 API URL 和 ZMQ 订阅地址
        run_execution_loop(
            exec_config.network.opinion_api_url,
            exec_config.network.zmq_exec_endpoint
        ).await;
    });

    // 5. 启动策略引擎
    // [修改] 将整个 config 传入 engine
    let strategy_config = config.clone();
    println!("🧠 [Strategy] Engine booting up...");
    let strategy_handle = tokio::task::spawn_blocking(move || {
        run_strategy_engine(strategy_config);
    });

    // 等待退出
    match strategy_handle.await {
        Ok(_) => println!("✅ [Main] Strategy Engine exited gracefully."),
        Err(e) => eprintln!("❌ [Main] Strategy Engine crashed: {:?}", e),
    }
}