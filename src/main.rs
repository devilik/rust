mod infrastructure;
mod model;
mod math;
mod gateway;
mod engine;
mod execution;
// 定义核心结构体 (Lib.rs 的内容可以直接放这里或者作为 mod)
// mod core; use core::*; 

use infrastructure::messaging::ZmqPublisher;
use gateway::poly_feed::run_poly_feed_handler;
use gateway::opinion_feed::run_opinion_chain_listener;
use engine::run_strategy_engine;
use execution::loop::run_execution_loop;

#[tokio::main]
async fn main() {
    println!("🚀 Starting Enterprise Market Maker System...");

    // 1. 启动 Polymarket 数据源 (生产者 -> 5555)
    tokio::spawn(async {
        let pub_sock = ZmqPublisher::new("tcp://*:5555");
        // 这里填入你要监听的 Polymarket Asset IDs
        let markets = vec!["217426331...".to_string()]; 
        run_poly_feed_handler(pub_sock, markets).await;
    });

    // 2. 启动 Opinion 链上监听 (可选生产者 -> 5555)
    tokio::spawn(async {
        let pub_sock = ZmqPublisher::new("tcp://*:5555"); // Pub Socket 可以多个
        run_opinion_chain_listener(pub_sock).await;
    });

    // 3. 启动执行引擎 (消费者 <- 5556)
    tokio::spawn(async {
        run_execution_loop().await;
    });

    // 4. 启动策略引擎 (大脑: 5555 -> 5556)
    // 它是 CPU 密集型死循环，使用 spawn_blocking 防止阻塞 tokio runtime
    let strategy_handle = tokio::task::spawn_blocking(|| {
        run_strategy_engine();
    });

    // 等待策略引擎 (实际上永远不会结束)
    strategy_handle.await.unwrap();
}