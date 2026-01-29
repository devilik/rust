// File: src/bin/data_recorder.rs

use std::fs::{OpenOptions, File};
use std::io::{Write, BufWriter};
use std::thread;
use std::time::Duration;
use std::str::FromStr;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use chrono::Utc;

// 引入你的项目模块
// 注意：需要在 Cargo.toml 里保证 binary 能访问 lib 的代码
use enterprise_market_maker::core::{OrderBookUpdate, Exchange}; 
use enterprise_market_maker::config::load_config;
use enterprise_market_maker::infrastructure::messaging::ZmqSubscriber;

struct MarketState {
    poly_mid: Decimal,
    opinion_last: Decimal,
}

fn main() {
    println!("📼 [Recorder] Starting Enterprise Data Recorder Service...");

    // 1. 加载配置 (为了获取 ZMQ 端口)
    let config = load_config("config.toml");
    let sub_endpoint = config.network.zmq_sub_endpoint; // 监听同一个端口

    // 2. 初始化 ZMQ 订阅者
    println!("🔗 Subscribing to Market Data Stream at: {}", sub_endpoint);
    let sub = ZmqSubscriber::new(&sub_endpoint, ""); // "" 订阅所有 topic

    // 3. 准备 CSV 文件 (使用 BufWriter 提高写入性能，减少磁盘 I/O 次数)
    let file_path = "backtest_data.csv";
    let file_exists = std::path::Path::new(file_path).exists();
    
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)
        .expect("❌ Failed to open backtest_data.csv");
        
    let mut writer = BufWriter::with_capacity(8 * 1024, file); // 8KB 缓存区

    // 如果是新文件，写入 Header
    if !file_exists {
        writeln!(writer, "timestamp,poly_price,opinion_price").unwrap();
        writer.flush().unwrap();
    }

    // 4. 状态缓存
    let mut state = MarketState {
        poly_mid: dec!(0),
        opinion_last: dec!(0),
    };

    let mut last_record_ts = 0;
    let mut recorded_count = 0;

    println!("🟢 [Recorder] Listening & Recording...");

    loop {
        // 非阻塞接收或轻微阻塞
        let msg = match sub.recv_raw_bytes() {
            Some(m) => m,
            None => {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
        };

        // 反序列化
        if let Ok(update) = bincode::deserialize::<OrderBookUpdate>(&msg) {
            let mut updated = false;

            match update.exchange {
                Exchange::Polymarket => {
                    let bid = update.bids.get(0).map(|x| x.0).unwrap_or(dec!(0));
                    let ask = update.asks.get(0).map(|x| x.0).unwrap_or(dec!(0));
                    if !bid.is_zero() && !ask.is_zero() {
                        let mid = (bid + ask) / dec!(2);
                        // 只有价格变动才标记更新
                        if (mid - state.poly_mid).abs() > dec!(0.0001) {
                            state.poly_mid = mid;
                            updated = true;
                        }
                    }
                },
                Exchange::OpinionLabs => {
                    // OpinionFeed 发送的 bids[0] 是最新成交价
                    if let Some(price) = update.bids.get(0) {
                        if !price.0.is_zero() && (price.0 - state.opinion_last).abs() > dec!(0.0001) {
                            state.opinion_last = price.0;
                            updated = true;
                        }
                    }
                },
                _ => {}
            }

            // 5. 写入逻辑
            // 条件：两个市场都有数据 && (有新价格变动 || 距离上次记录超过1秒)
            let now = Utc::now().timestamp_millis();
            
            if !state.poly_mid.is_zero() && !state.opinion_last.is_zero() {
                if updated || (now - last_record_ts > 1000) {
                    
                    if let Err(e) = writeln!(
                        writer, 
                        "{},{},{}", 
                        now, state.poly_mid, state.opinion_last
                    ) {
                        eprintln!("❌ Write Error: {}", e);
                    }

                    last_record_ts = now;
                    recorded_count += 1;

                    // 每 100 条数据刷入硬盘一次 (防止断电全丢，也不至于太慢)
                    if recorded_count % 100 == 0 {
                        let _ = writer.flush();
                        // 打印进度条，证明活着
                        print!("\r📼 Recorded: {} ticks | P: {} | O: {}", recorded_count, state.poly_mid, state.opinion_last);
                    }
                }
            }
        }
    }
}