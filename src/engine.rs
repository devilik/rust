// File: src/engine.rs

use std::thread;
use std::sync::{mpsc, Arc, atomic::{AtomicBool, Ordering}};
use std::fs;
use std::time::Duration;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use rust_decimal::Decimal; 

// 引入核心模块
use crate::core::{OrderBookUpdate, InventoryUpdate, TradeSignal, Exchange, Side};
use crate::model::as_logic::{OpinionGridStrategy, PersistState}; 
use crate::model::risk::RiskManager;
use crate::infrastructure::messaging::{ZmqSubscriber, ZmqPublisher};
use crate::config::AppConfig;
use chrono::Utc;

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

fn load_initial_state(file_path: &str) -> (f64, f64) {
    if let Ok(content) = fs::read_to_string(file_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
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

    // [新增] 报价节流器 & 价格缓存
    let mut last_bid = dec!(0);
    let mut last_ask = dec!(0);
    
    // [关键修改] 分别记录不同市场的价格
    let mut poly_mid_price = dec!(0);
    let mut opinion_last_price = dec!(0);

    println!("🧠 [Engine] Active. Source: {} | Realized Inv: {} | Cash: ${:.2}", 
        app_config.strategy.pricing_source, init_inv, init_cash);

    while running.load(Ordering::SeqCst) {
        let msg = match sub.recv_raw_bytes() {
            Some(m) => m,
            None => {
                thread::sleep(Duration::from_millis(1));
                continue; 
            }
        };

        // --- A. 处理行情 (Market Data) ---
        if let Ok(update) = bincode::deserialize::<OrderBookUpdate>(&msg) {
            
            // 1. 根据数据源更新价格缓存
            match update.exchange {
                Exchange::Polymarket => {
                    let best_bid = update.bids.get(0).map(|x| x.0).unwrap_or(dec!(0));
                    let best_ask = update.asks.get(0).map(|x| x.0).unwrap_or(dec!(0));
                    if !best_bid.is_zero() && !best_ask.is_zero() {
                        poly_mid_price = (best_bid + best_ask) / dec!(2);
                    }
                },
                Exchange::OpinionLabs => {
                    // OpinionFeed 发送过来的"虚拟盘口" bids[0] 就是 last price
                    if let Some(price) = update.bids.get(0) {
                        if !price.0.is_zero() {
                            opinion_last_price = price.0;
                        }
                    }
                },
                _ => {}
            }

            // 2. [核心决策] 选取锚定价格 (Anchor Price)
            let anchor_price = match app_config.strategy.pricing_source.as_str() {
                "opinion" => {
                    // 如果还没有收到 Opinion 的成交价，暂时跳过或者使用 Poly 兜底
                    if opinion_last_price.is_zero() { 
                        if !poly_mid_price.is_zero() { poly_mid_price } else { continue; }
                    } else {
                        opinion_last_price
                    }
                },
                _ => {
                    // 默认使用 Polymarket
                    if poly_mid_price.is_zero() { continue; }
                    poly_mid_price
                }
            };
            
            let anchor_f64 = anchor_price.to_f64().unwrap_or(0.0);

            // 3. 风控检查 (PnL Check)
            // 注意：计算权益变化最好总是使用"当前市场公允价"，这里我们也可以用 anchor_price
            let pnl_change = strategy.calculate_equity_change(anchor_f64);
            if risk_manager.update_pnl_and_check_kill(pnl_change) {
                println!("🛑 System Halted due to Risk Trigger.");
                send_emergency_cancel(&pub_sock);
                break; 
            }

            // 4. 计算策略报价
            // 将 anchor_price 传入模型
            let market_ts_ms = update.timestamp_ns / 1_000_000;
            let (new_bid, new_ask) = strategy.calculate_quotes(anchor_price, market_ts_ms);

            // 5. 报价过滤器 (Quote Filter) - 防止微小波动频繁撤单
            let tick = app_config.strategy.tick_size; 
            let tick_dec = Decimal::try_from(tick).unwrap_or(dec!(0.01));
            
            let diff_bid = (new_bid - last_bid).abs();
            let diff_ask = (new_ask - last_ask).abs();
            
            // 只有变化超过半个 tick 才更新
            if diff_bid < (tick_dec / dec!(2)) && diff_ask < (tick_dec / dec!(2)) {
                continue;
            }
            last_bid = new_bid;
            last_ask = new_ask;

            // 6. 构建信号
            let now_ns = Utc::now().timestamp_nanos();
            let size_f64 = app_config.strategy.default_order_size_usd; 
            let size_usd = Decimal::try_from(size_f64).unwrap_or(dec!(10));

            // 注意：Opinion 市场 ID 应该从 Config 读取目标 ID，而不是直接用 Update 里的 ID
            // 因为 Update 可能是 Poly 的 ID。我们要往 Opinion 发单。
            let target_symbol_id = app_config.markets.target_market_id;

            let signals = vec![
                TradeSignal {
                    strategy_id: 1,
                    target_exchange: Exchange::OpinionLabs,
                    symbol_id: target_symbol_id, // 修正：始终针对目标市场发单
                    side: Side::Buy,
                    price: new_bid,
                    size_usd,
                    logic_tag: 1,
                    created_at_ns: now_ns,
                },
                TradeSignal {
                    strategy_id: 1,
                    target_exchange: Exchange::OpinionLabs,
                    symbol_id: target_symbol_id,
                    side: Side::Sell,
                    price: new_ask,
                    size_usd,
                    logic_tag: 1,
                    created_at_ns: now_ns,
                }
            ];

            // 7. 发送并 [乐观登记]
            for signal in signals {
                // 再次检查价格是否离谱 (针对 anchor_price 的硬性保护)
                // 防止模型算出负数或者极大值
                if signal.price <= dec!(0.01) || signal.price >= dec!(0.99) {
                    continue;
                }

                if risk_manager.check_signal(&signal) {
                    // 登记 Pending
                    strategy.on_signal_created(signal.side, signal.price, signal.size_usd);
                    // 发送
                    pub_sock.send_signal(&signal);
                }
            }
        } 
        // --- B. 处理成交/库存更新 (Inventory Sync) ---
        else if let Ok(inv_update) = bincode::deserialize::<InventoryUpdate>(&msg) {
            // 收到成交回报 -> 更新真实库存 -> 核销 Pending
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