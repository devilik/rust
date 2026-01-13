// File: src/engine.rs

use std::thread;
use std::sync::{mpsc, Arc, atomic::{AtomicBool, Ordering}};
use std::fs;
use std::time::Duration;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;

// 引入核心模块
use crate::core::{OrderBookUpdate, InventoryUpdate, TradeSignal, Exchange, Side};
use crate::model::as_logic::{OpinionGridStrategy, StrategyConfig, PersistState};
use crate::model::risk::RiskManager;
use crate::infrastructure::messaging::{ZmqSubscriber, ZmqPublisher};
use crate::config::{AppConfig, RiskConfig};
use crate::model::as_logic::{OpinionGridStrategy, StrategyConfig};
// --- [Part 1] IO Worker: 异步持久化 ---
// 这个函数会在后台启动一个线程，专门负责把策略状态写入硬盘
fn spawn_persistence_worker(file_path: String) -> mpsc::Sender<PersistState> {
    let (tx, rx) = mpsc::channel::<PersistState>();

    thread::spawn(move || {
        println!("💾 [IO Worker] Monitoring state file: {}", file_path);
        
        // 循环接收来自策略线程的状态更新
        loop {
            // 阻塞等待，直到有数据发过来
            let mut latest_state = match rx.recv() {
                Ok(s) => s,
                Err(_) => break, // 通道关闭，线程退出
            };

            // ⚡ 排水机制 (Draining): 
            // 如果积压了多条更新 (比如高频成交时)，只取最后一条最新的状态写入
            // 这是防止 IO 瓶颈的关键
            while let Ok(newer_state) = rx.try_recv() {
                latest_state = newer_state;
            }

            // 序列化并写入临时文件
            let json = serde_json::json!({
                "inventory_shares": latest_state.inventory_shares,
                "cash_balance": latest_state.cash_balance,
                "timestamp": latest_state.timestamp
            });
            
            // 原子写入: write -> rename，防止断电导致文件损坏
            let temp_path = format!("{}.tmp", file_path);
            if let Ok(content) = serde_json::to_string(&json) {
                if fs::write(&temp_path, content).is_ok() {
                    let _ = fs::rename(&temp_path, &file_path);
                }
            }
        }
    });

    tx
}

// 辅助函数: 系统启动时读取初始状态
fn load_initial_state(file_path: &str) -> (f64, f64) {
    if let Ok(content) = fs::read_to_string(file_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
            let inv = v["inventory_shares"].as_f64().unwrap_or(0.0);
            let cash = v["cash_balance"].as_f64().unwrap_or(0.0);
            return (inv, cash);
        }
    }
    // 如果文件不存在，默认从 0 开始
    (0.0, 0.0)
}

// --- [Main] 策略引擎主函数 ---
pub fn run_strategy_engine(app_config: AppConfig) {
    // 1. 设置优雅退出信号 (Graceful Shutdown)
    // 使用 AtomicBool 在不同线程间共享运行状态
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // 捕获 Ctrl+C 信号 (需要 cargo.toml 添加 `ctrlc` 依赖)
    // 如果没有 ctrlc 库，可以手动用其他方式触发，或者依赖外部关闭 channel
    if let Err(e) = ctrlc::set_handler(move || {
        println!("\n🛑 [SIGINT] Received Ctrl+C! Initiating Graceful Shutdown...");
        r.store(false, Ordering::SeqCst);
    }) {
        eprintln!("⚠️ Warning: Failed to set Ctrl-C handler: {}", e);
    }

    // 2. 初始化网络层
    // Sub: 接收行情 (Feed) 和 成交回报 (Execution)
    let sub = ZmqSubscriber::new(&app_config.network.zmq_sub_endpoint, "");
    // Pub: 发送交易信号 (Signals)
    let pub_sock = ZmqPublisher::new(&app_config.network.zmq_exec_endpoint);
    // 3. 初始化持久化层
    let state_file = "./data/strategy_state.json".to_string();
    let _ = fs::create_dir_all("./data");
    
    // 启动 IO 线程
    let persist_tx = spawn_persistence_worker(state_file.clone());
    // 加载历史账本
    let (init_inv, init_cash) = load_initial_state(&state_file);

    // 4. 初始化策略模块 (手工参数配置)
    let strategy_config = app_config.strategy.clone();
    
    // 注入持久化通道
    let mut strategy = OpinionGridStrategy::new(strategy_config, Some(persist_tx));
    // 恢复之前的“真金白银”状态
    strategy.restore_state(init_inv, init_cash);

    // 5. 初始化风控模块 (Part 4)
    let mut risk_manager = RiskManager::new(app_config.risk.clone());

    println!("🧠 [Engine] Active. Cash Ledger: ${:.2} | Inventory: {}", init_cash, init_inv);

    // --- 主循环 ---
    while running.load(Ordering::SeqCst) {
        // 尝试接收消息 (非阻塞或带超时，以便能响应 Ctrl+C)
        // 假设 recv_raw_bytes 内部是阻塞的，建议在 ZmqSubscriber 实现里加 timeout
        // 这里为了代码通用性，假设它能正常返回
        let msg = match sub.recv_raw_bytes() {
            Some(m) => m,
            None => {
                // 没有消息时短暂休眠，避免 CPU 空转
                // 实际高频场景中 ZMQ 会处理得很好，这里是为了安全演示
                thread::sleep(Duration::from_millis(1));
                continue; 
            }
        };

        // --- 分支 A: 处理行情更新 (Market Data) ---
        if let Ok(update) = bincode::deserialize::<OrderBookUpdate>(&msg) {
            // A1. 计算中间价
            let best_bid = update.bids.get(0).map(|x| x.0).unwrap_or(dec!(0));
            let best_ask = update.asks.get(0).map(|x| x.0).unwrap_or(dec!(0));
            
            // 如果数据异常 (0报价)，跳过
            if best_bid.is_zero() || best_ask.is_zero() { continue; }
            let mid_price = (best_bid + best_ask) / dec!(2);
            let mid_f64 = mid_price.to_f64().unwrap_or(0.0);

            // A2. [关键] 实时风控检查 (Mark-to-Market PnL)
            // 即使没有成交，价格变动也会导致持仓市值变化，必须实时计算回撤
            let pnl_change = strategy.calculate_equity_change(mid_f64);
            
            if risk_manager.update_pnl_and_check_kill(pnl_change) {
                // 🚨 触发熔断！
                println!("🛑 System Halted due to Risk Trigger (Drawdown Limit).");
                send_emergency_cancel(&pub_sock);
                break; // 立即跳出循环，停止策略
            }

            // A3. 计算策略报价 (AS Model Logic)
            let (new_bid, new_ask) = strategy.calculate_quotes(mid_price);

            // A4. 构建交易信号
            let now_ns = chrono::Utc::now().timestamp_nanos();
            let size_f64 = strategy.cfg.default_order_size_usd; 
            let size_usd = Decimal::try_from(size_f64).unwrap_or(dec!(50));

            // 双边报价 (Bid & Ask)
            let signals = vec![
                TradeSignal {
                    strategy_id: 1,
                    target_exchange: Exchange::OpinionLabs,
                    symbol_id: update.symbol_id, // 需注意 ID 映射，这里简化为直接使用
                    side: Side::Buy,
                    price: new_bid,
                    size_usd,
                    logic_tag: 1,
                    created_at_ns: now_ns,
                },
                TradeSignal {
                    strategy_id: 1,
                    target_exchange: Exchange::OpinionLabs,
                    symbol_id: update.symbol_id,
                    side: Side::Sell,
                    price: new_ask,
                    size_usd,
                    logic_tag: 1,
                    created_at_ns: now_ns,
                }
            ];

            // A5. 发送前风控审查 (Pre-Trade Check)
            for signal in signals {
                // 只有通过风控检查的信号才会被发送
                if risk_manager.check_signal(&signal) {
                    pub_sock.send_signal(&signal);
                }
            }
        } 
        // --- 分支 B: 处理成交/库存更新 (Fills) ---
        else if let Ok(inv_update) = bincode::deserialize::<InventoryUpdate>(&msg) {
            // B1. 更新策略状态 (这是最真实的账本更新)
            // inv_update.cost_usd 必须是真实的现金流 (Gateway 层计算)
            strategy.on_fill(inv_update.change, inv_update.cost_usd);
            
            println!("💵 [Fill Confirmed] Cash: ${:.2} | Inv: {} | Delta Cost: ${:.2}", 
                strategy.current_cash_balance, 
                strategy.current_inventory_shares,
                inv_update.cost_usd
            );
            
            // 注意：这里不需要显式调用 risk_manager 更新 PnL
            // 因为下一次行情到来时，calculate_equity_change 会自动基于最新的 Cash 和 Inv 计算出准确的权益
        }
    }

    // --- 退出清理逻辑 (Post-Loop) ---
    // 无论是 Ctrl+C 还是 熔断退出，都会执行这里
    println!("🧹 [Shutdown] Engine stopped. Sending EMERGENCY CANCEL ALL...");
    
    // 发送多次以防丢包
    for _ in 0..3 {
        send_emergency_cancel(&pub_sock);
        thread::sleep(Duration::from_millis(100));
    }
    
    println!("👋 [Shutdown] Graceful exit complete.");
}

// 辅助函数: 发送紧急撤单信号 (Kill Switch Signal)
fn send_emergency_cancel(pub_sock: &ZmqPublisher) {
    let kill_signal = TradeSignal {
        strategy_id: 0,
        target_exchange: Exchange::OpinionLabs,
        symbol_id: 0, // 0 通常约定为 Wildcard (所有市场)
        side: Side::Buy, // 占位符
        price: dec!(0),
        size_usd: dec!(0),
        logic_tag: 99, // <--- 99 号令：执行层识别为“全部撤单”
        created_at_ns: chrono::Utc::now().timestamp_nanos(),
    };
    pub_sock.send_signal(&kill_signal);
}