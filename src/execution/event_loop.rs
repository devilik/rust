// File: src/execution/loop.rs

use crate::infrastructure::messaging::ZmqSubscriber;
use crate::execution::opinion_maker::{OpinionMakerGateway, SignedOrder};
use crate::core::TradeSignal;
use std::sync::Arc;
use tokio::sync::mpsc; // 使用 Tokio 的异步通道
use std::time::Duration;

pub async fn run_execution_loop(api_url: String, zmq_endpoint: String) {
    // 1. 初始化 ZMQ 订阅者 (监听 "SG" 也就是 Signal 信号)
    let sub = ZmqSubscriber::new(&zmq_endpoint, "SG");
    
    // 从环境变量读取私钥 (生产环境安全做法)
    let pk = std::env::var("PRIVATE_KEY").unwrap_or("0xYOUR_PRIVATE_KEY_HERE".to_string());
    
    // 初始化 Gateway (复用 HTTP Client)
    let gateway = Arc::new(OpinionMakerGateway::new(&pk, &api_url));
    println!("🔫 [Execution] Ready. Listening for signals...");

    // ------------------------------------------------------------------
    // 🌊 流水线 Part A: 广播员 (Broadcaster) - IO 密集型
    // ------------------------------------------------------------------
    // 创建一个缓冲区为 1000 的通道。如果网络卡顿，积压超过 1000 个订单则开始丢弃，防止内存爆掉
    let (tx, mut rx) = mpsc::channel::<SignedOrder>(1000);

    let gateway_io = gateway.clone();
    tokio::spawn(async move {
        println!("📡 [Broadcaster] Online... (Pipeline Started)");
        
        // 持续从通道里接收“已签名”的订单
        while let Some(signed_order) = rx.recv().await {
            let gw = gateway_io.clone();
            
            // 🔥 并发发送：对每个订单都开一个轻量级 Task
            // 依赖 HTTP Keep-Alive 和 connection pooling 来管理 TCP 连接
            tokio::spawn(async move {
                // 这里的 submit_order 是纯网络请求
                match gw.submit_order(signed_order).await {
                    Ok(_id) => {
                        // 高频模式下建议关闭普通日志，减少 IO 开销
                        // println!("✅ Sent: {}", id); 
                    },
                    Err(e) => {
                        // 只打印错误日志
                        eprintln!("❌ Send Error: {}", e);
                    }
                }
            });
        }
    });

    // ------------------------------------------------------------------
    // ✍️ 流水线 Part B: 签名员 (Signer) - CPU 密集型 & 主循环
    // ------------------------------------------------------------------
    loop {
        // 阻塞接收 ZMQ 消息
        // (注: 真实场景如果想响应 Ctrl+C 退出，可以在 ZMQ 层做非阻塞处理，
        // 但这里为了代码清晰，假设接收到 Kill Signal 后由 Gateway 负责清理)
        if let Some(msg) = sub.recv_raw_bytes() {
            if let Ok(signal) = bincode::deserialize::<TradeSignal>(&msg) {
                
                // 🛑 优先级 0: 熔断信号检查 (Kill Switch)
                // 必须在签名之前检查，确保最高优先级处理
                if signal.logic_tag == 99 {
                    let gw_cancel = gateway.clone();
                    
                    // 立即启动一个独立任务去执行撤单
                    tokio::spawn(async move {
                        
                        // ♻️ 重试机制：尝试 3 次，防止网络抖动导致撤单失败
                        for i in 1..=3 {
                            match gw_cancel.cancel_all().await {
                                Ok(_) => {
                                    println!("✅ [EXEC] Emergency Cancel SUCCESS (Attempt {})", i);
                                    break; // 成功即退出
                                },
                                Err(e) => {
                                    eprintln!("❌ [EXEC] Cancel Failed (Attempt {}): {:?}", i, e);
                                    // 失败稍微等一下再试
                                    tokio::time::sleep(Duration::from_millis(200)).await;
                                }
                            }
                        }
                    });
                    
                    // 收到熔断信号后，跳过当前循环，不处理后续逻辑
                    continue; 
                }

                // 🚀 优先级 1: 正常订单处理
                let gw_signer = gateway.clone();
                let tx_inner = tx.clone();
                
                // 为了不阻塞 ZMQ 接收下一个信号，我们将“签名”也放入 Task 中
                // 这样即使签名需要 1ms，也不会阻碍我们接收下一个行情信号
                tokio::spawn(async move {
                    // 1. 生成 EIP-712 签名 (CPU 计算)
                    // create_signed_order 需要在 opinion_maker.rs 中实现 (参考 Part 2)
                    match gw_signer.create_signed_order(signal).await {
                        Ok(signed) => {
                            // 2. 将签名好的包扔进通道，交给 Broadcaster 发送
                            // 如果通道满了 (Backpressure)，选择丢弃该订单，而不是阻塞
                            if let Err(_) = tx_inner.send(signed).await {
                                eprintln!("⚠️ [EXEC] Pipeline full! Dropping order to preserve latency.");
                            }
                        },
                        Err(e) => {
                            eprintln!("⚠️ [EXEC] Signing Failed: {:?}", e);
                        }
                    }
                });
            }
        }
    }
}