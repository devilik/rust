use ethers::prelude::*;
use ethers::providers::{Provider, Ws};
use crate::infrastructure::messaging::ZmqPublisher;
use market_maker_core::{OrderBookUpdate, Exchange};
use std::sync::Arc;

pub async fn run_opinion_chain_listener(zmq_pub: ZmqPublisher) {
    // 1. 连接 Alchemy 的 WSS 节点 (必须是 WSS)
    let ws_url = "wss://polygon-mainnet.g.alchemy.com/v2/YOUR_API_KEY";
    let provider = Provider::<Ws>::connect(ws_url).await.expect("RPC Connect Error");
    let provider = Arc::new(provider);

    // 2. 定义我们要听什么事件
    // 假设这是 Opinion 核心合约地址
    let contract_addr: Address = "0x123456...".parse().unwrap();
    
    // 过滤条件：只听这个合约产生的 "OrderMatched" 事件
    let filter = Filter::new()
        .address(contract_addr)
        .event("OrderMatched(bytes32,uint256)"); // ABI 签名

    println!("👂 [Gateway] Listening to Opinion Labs Blockchain Events...");

    // 3. 订阅 (Subscribe) - 这里的 stream 就是推流
    let mut stream = provider.subscribe_logs(&filter).await.unwrap();

    // 4. 事件循环
    while let Some(log) = stream.next().await {
        // ⚡️ 收到 Log，说明链上刚刚成交了一笔！
        println!("⚡ [Gateway] On-Chain Trade Detected! Tx: {:?}", log.transaction_hash);

        // 构造一个伪造的 OrderBookUpdate 通知策略引擎去查库存
        // 或者直接在这里解析 Log 里的 amount 更新库存
        let update = OrderBookUpdate {
            exchange: Exchange::OpinionLabs,
            symbol_id: 0, 
            timestamp_ns: chrono::Utc::now().timestamp_nanos(),
            bids: smallvec![], // 链上事件通常不带盘口，只带成交
            asks: smallvec![],
        };
        
        zmq_pub.send_book_update(&update);
    }
}