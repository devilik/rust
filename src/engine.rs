// File: src/engine.rs

use std::thread;
use std::sync::{mpsc, Arc, atomic::{AtomicBool, Ordering}};
use std::fs;
use std::time::Duration;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use rust_decimal::Decimal; // 确保引入 Decimal

// 引入核心模块
use crate::core::{OrderBookUpdate, InventoryUpdate, TradeSignal, Exchange, Side};
use crate::model::as_logic::{OpinionGridStrategy, PersistState}; // 引入 PersistState
use crate::model::risk::RiskManager;
use crate::infrastructure::messaging::{ZmqSubscriber, ZmqPublisher};
use crate::config::AppConfig;

// --- [Part 1] IO Worker: 异步持久化 ---
fn spawn_persistence_worker(file_path: String) -> mpsc::Sender<PersistState> {
    let (tx, rx) = mpsc::channel::<PersistState>();

    thread::spawn(move || {
        println!("💾 [IO Worker] Monitoring state file: {}", file_path);
        
        loop {
            let mut latest_state = match rx.recv() {
                Ok(s) => s,
                Err(_) => break, 
            };

            while let Ok(newer_state) = rx.try_recv() {
                latest_state = newer_state;
            }

            // [修改点 1] 字段名改为 realized_inventory
            let json = serde_json::json!({
                "realized_inventory": latest_state.realized_inventory, 
                "cash_balance": latest_state.cash_balance,
                "timestamp": latest_state.timestamp
            });
            
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

// [修改点 2] 读取初始状态时，读取 realized_inventory
fn load_initial_state(file_path: &str) -> (f64, f64) {
    if let Ok(content) = fs::read_to_string(file_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
            // 注意这里读的是 realized_inventory
            let inv = v["realized_inventory"].as_f64().unwrap_or(0.0);
            let cash = v["cash_balance"].as_f64().unwrap_or(0.0);
            return (inv, cash);
        }
    }
    (0.0, 0.0)
}

// --- [Main] 策略引擎主函数 ---
pub fn run_strategy_engine(app_config: AppConfig) {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    if let Err(e) = ctrlc::set_handler(move || {
        println!("\n🛑 [SIGINT] Received Ctrl+C! Initiating Graceful Shutdown...");
        r.store(false, Ordering::SeqCst);
    }) {
        eprintln!("⚠️ Warning: Failed to set Ctrl-C handler: {}", e);
    }

    let sub = ZmqSubscriber::new(&app_config.network.zmq_sub_endpoint, "");
    let pub_sock = ZmqPublisher::new(&app_config.network.zmq_exec_endpoint);
    
    let state_file = "./data/strategy_state.json".to_string();
    let _ = fs::create_dir_all("./data");
    
    let persist_tx = spawn_persistence_worker(state_file.clone());
    let (init_inv, init_cash) = load_initial_state(&state_file);

    // 初始化策略
    let mut strategy = OpinionGridStrategy::new(app_config.strategy.clone(), Some(persist_tx));
    strategy.restore_state(init_inv, init_cash);

    let mut risk_manager = RiskManager::new(app_config.risk);

    // [新增] 报价节流器
    let mut last_bid = dec!(0);
    let mut last_ask = dec!(0);

    println!("🧠 [Engine] Active. Realized Inv: {} | Cash: ${:.2}", init_inv, init_cash);

    while running.load(Ordering::SeqCst) {
        let msg = match sub.recv_raw_bytes() {
            Some(m) => m,
            None => {
                thread::sleep(Duration::from_millis(1));
                continue; 
            }
        };

        // --- A. 处理行情 ---
        if let Ok(update) = bincode::deserialize::<OrderBookUpdate>(&msg) {
            let best_bid = update.bids.get(0).map(|x| x.0).unwrap_or(dec!(0));
            let best_ask = update.asks.get(0).map(|x| x.0).unwrap_or(dec!(0));
            
            if best_bid.is_zero() || best_ask.is_zero() { continue; }
            let mid_price = (best_bid + best_ask) / dec!(2);
            let mid_f64 = mid_price.to_f64().unwrap_or(0.0);

            // 1. 风控检查
            let pnl_change = strategy.calculate_equity_change(mid_f64);
            if risk_manager.update_pnl_and_check_kill(pnl_change) {
                println!("🛑 System Halted due to Risk Trigger.");
                send_emergency_cancel(&pub_sock);
                break; 
            }

            // 2. 计算策略报价 (内部已使用 Effective Inventory)
            let market_ts_ms = update.timestamp_ns / 1_000_000;

            // [修改] 将时间戳传入策略
            let (new_bid, new_ask) = strategy.calculate_quotes(mid_price, market_ts_ms);

            // 3. 报价过滤器 (Quote Filter)
            let tick = app_config.strategy.tick_size; // 需确保 config 里是 f64
            let tick_dec = Decimal::try_from(tick).unwrap_or(dec!(0.01));
            let diff_bid = (new_bid - last_bid).abs();
            let diff_ask = (new_ask - last_ask).abs();
            
            // 只有价格变动超过半个 tick 才更新，避免刷单
            if diff_bid < (tick_dec / dec!(2)) && diff_ask < (tick_dec / dec!(2)) {
                continue;
            }
            last_bid = new_bid;
            last_ask = new_ask;

            // 4. 构建信号
            let now_ns = chrono::Utc::now().timestamp_nanos();
            // 注意：这里需要从 config 读取默认下单金额
            let size_f64 = app_config.strategy.default_order_size_usd; 
            let size_usd = Decimal::try_from(size_f64).unwrap_or(dec!(10));

            let signals = vec![
                TradeSignal {
                    strategy_id: 1,
                    target_exchange: Exchange::OpinionLabs,
                    symbol_id: update.symbol_id,
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

            // 5. 发送并 [乐观登记]
            for signal in signals {
                if risk_manager.check_signal(&signal) {
                    // [修改点 3] 先在策略里“登记”这笔单子 (更新 Pending)
                    strategy.on_signal_created(signal.side, signal.price, signal.size_usd);
                    
                    // 然后再发出去
                    pub_sock.send_signal(&signal);
                }
            }
        } 
        // --- B. 处理成交/库存更新 ---
        else if let Ok(inv_update) = bincode::deserialize::<InventoryUpdate>(&msg) {
            // [修改点 4] 确认成交，核销 Pending，更新 Realized
            // 注意：这需要 Gateway 发送的是增量 (Change)，而不是总量
            strategy.on_fill_confirmed(inv_update.change, inv_update.cost_usd);
            
            println!("⚖️ [Inventory Sync] Realized: {:.2} | Pending: {:.2} | Eff: {:.2}", 
                strategy.realized_inventory,
                strategy.pending_inventory,
                strategy.get_effective_inventory()
            );
        }
    }

    println!("🧹 [Shutdown] Engine stopped. Sending EMERGENCY CANCEL ALL...");
    for _ in 0..3 {
        send_emergency_cancel(&pub_sock);
        thread::sleep(Duration::from_millis(100));
    }
}

fn send_emergency_cancel(pub_sock: &ZmqPublisher) {
    let kill_signal = TradeSignal {
        strategy_id: 0,
        target_exchange: Exchange::OpinionLabs,
        symbol_id: 0, 
        side: Side::Buy, 
        price: dec!(0),
        size_usd: dec!(0),
        logic_tag: 99, 
        created_at_ns: chrono::Utc::now().timestamp_nanos(),
    };
    pub_sock.send_signal(&kill_signal);
}