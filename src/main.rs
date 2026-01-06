mod infrastructure;
mod model;
mod math;
mod gateway;
mod engine;
// ⚠️ 注意：Rust 中 'loop' 是关键字，如果文件名是 loop.rs，在 mod 声明或 use 时需要用 r#loop
// 确保你有 src/execution/mod.rs 文件，并在其中写了 pub mod r#loop;
mod execution;
// ✅ 必须启用 core 模块，因为 OrderBookUpdate 等结构体定义在这里
mod core; 

use infrastructure::messaging::ZmqPublisher;
use gateway::poly_feed::run_poly_feed_handler;
use gateway::opinion_feed::run_opinion_chain_listener;
use engine::run_strategy_engine;
// ✅ 修复：使用 r#loop 导入 loop 模块
use execution::event_loop::run_execution_loop;

#[tokio::main]
async fn main() {
    println!("🚀 Starting Enterprise Market Maker System...");

    // [关键修复] 创建共享的 ZMQ 发布者
    // 不能调用两次 new("tcp://*:5555")，否则第二个会因为端口占用而崩溃
    // ZmqPublisher 实现了 Clone (基于 Arc)，可以在多个任务间共享同一个 socket
    let market_data_pub = ZmqPublisher::new("tcp://*:5555");

    // 1. 启动 Polymarket 数据源 (生产者 -> 5555)
    let poly_pub = market_data_pub.clone();
    tokio::spawn(async move {
        // 这里填入你要监听的 Polymarket Asset IDs
        let markets = vec!["217426331...".to_string()]; 
        println!("👂 [PolyFeed] Starting listener for {} markets...", markets.len());
        run_poly_feed_handler(poly_pub, markets).await;
    });

    // 2. 启动 Opinion 链上监听 (生产者 -> 5555)
    // 复用同一个端口发布 Opinion 的数据
    let opinion_pub = market_data_pub.clone();
    tokio::spawn(async move {
        println!("👂 [OpinionFeed] Starting chain listener...");
        run_opinion_chain_listener(opinion_pub).await;
    });

    // 3. 启动执行引擎 (消费者 <- 5556)
    // 它负责接收策略引擎发出的 "SG" 信号并下单
    tokio::spawn(async {
        println!("🔫 [Execution] Starting execution loop...");
        run_execution_loop().await;
    });

    // 4. 启动策略引擎 (大脑: Sub 5555 -> Pub 5556)
    // 它是 CPU 密集型死循环，使用 spawn_blocking 防止阻塞 tokio runtime
    println!("🧠 [Strategy] Engine booting up...");
    let strategy_handle = tokio::task::spawn_blocking(|| {
        run_strategy_engine();
    });

    // 等待策略引擎 (它内部有 Ctrl+C 处理，退出时会返回)
    match strategy_handle.await {
        Ok(_) => println!("✅ [Main] Strategy Engine exited gracefully."),
        Err(e) => eprintln!("❌ [Main] Strategy Engine crashed: {:?}", e),
    }

    println!("👋 [Main] System Shutdown Complete.");
}