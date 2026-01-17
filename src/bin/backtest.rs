// File: src/bin/backtest.rs

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use rust_decimal::prelude::ToPrimitive;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::str::FromStr;

// 引入你的项目模块 (根据 Cargo.toml 的 package name: enterprise_market_maker)
use enterprise_market_maker::model::as_logic::{OpinionGridStrategy, StrategyConfig};

// 定义一行 CSV 数据结构
struct MarketTick {
    timestamp: i64,
    mid_price: Decimal,
    best_bid: Decimal,
    best_ask: Decimal,
}

fn main() {
    println!("⏳ 初始化回测引擎...");

    // 1. 配置回测参数 (与实盘 config 类似，但针对回测调整)
    let config = StrategyConfig {
        risk_aversion_gamma: 0.05,
        liquidity_k: 5000.0,
        min_spread_bps: 50,    // 50 bps spread
        tick_size: 0.01,       // 价格最小变动
        max_inventory_usd: 2000.0,
        default_order_size_usd: 10.0,
        vol_window_size: 20,   // 回测数据少时，窗口改小一点以便快速启动
        maturity_timestamp_ms: 1767139200000, // 设置一个很远的未来时间 (2025/12/31)
        terminal_dumping_factor: 10.0,
        closing_window_seconds: 3600,
    };

    // 初始化策略 (传入 None 表示不需要 ZMQ/文件持久化)
    let mut strategy = OpinionGridStrategy::new(config.clone(), None);

    // 2. 加载历史数据
    // 假设数据文件在项目根目录下，名为 data.csv
    let file = File::open("data.csv").expect("❌ 找不到 data.csv 文件，请先创建！");
    let reader = BufReader::new(file);

    // 状态追踪
    let mut my_last_bid = dec!(0);
    let mut my_last_ask = dec!(0);
    let mut total_profit = 0.0;
    let mut trade_count = 0;
    
    println!("-------------------------------------------------------------");
    println!("时间戳 | 市场价 (Bid/Ask) | 我的报价 (Bid/Ask) | 库存 | 动作");
    println!("-------------------------------------------------------------");

    // 3. 逐行读取并回测
    for (index, line) in reader.lines().enumerate() {
        let line = line.unwrap();
        if index == 0 && line.contains("timestamp") { continue; } // 跳过 CSV 表头

        // 解析 CSV (简单解析，生产环境可用 csv crate)
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 4 { continue; }

        let tick = MarketTick {
            timestamp: parts[0].trim().parse().unwrap_or(0),
            mid_price: Decimal::from_str(parts[1].trim()).unwrap_or(dec!(0)),
            best_bid: Decimal::from_str(parts[2].trim()).unwrap_or(dec!(0)),
            best_ask: Decimal::from_str(parts[3].trim()).unwrap_or(dec!(0)),
        };

        // --- A. 模拟撮合 (Matching Engine) ---
        // 核心逻辑：用 T-1 时刻的挂单，去匹配 T 时刻的市场价格
        // 注意：做市商是 Maker，被动成交。
        
        // 1. 卖单成交判定：如果市场有人愿意以比我高的价格买 (Market Bid >= My Ask)
        if !my_last_ask.is_zero() && tick.best_bid >= my_last_ask {
            let fill_price = my_last_ask;
            let fill_val = config.default_order_size_usd;
            let shares = fill_val / fill_price.to_f64().unwrap();
            
            // 策略回调：库存减少，现金增加
            strategy.on_fill_confirmed(-shares, fill_val);
            
            total_profit += strategy.calculate_equity_change(tick.mid_price.to_f64().unwrap());
            trade_count += 1;
            println!("✅ [SELL] 成交价: {} | 库存: {:.2} | 获利更新", fill_price, strategy.realized_inventory);
        }

        // 2. 买单成交判定：如果市场有人愿意以比我低的价格卖 (Market Ask <= My Bid)
        else if !my_last_bid.is_zero() && tick.best_ask <= my_last_bid {
            let fill_price = my_last_bid;
            let fill_val = config.default_order_size_usd;
            let shares = fill_val / fill_price.to_f64().unwrap();
            
            // 策略回调：库存增加，现金减少
            strategy.on_fill_confirmed(shares, -fill_val);

            total_profit += strategy.calculate_equity_change(tick.mid_price.to_f64().unwrap());
            trade_count += 1;
            println!("✅ [BUY]  成交价: {} | 库存: {:.2} | 获利更新", fill_price, strategy.realized_inventory);
        }

        // --- B. 策略计算 (Strategy Calculation) ---
        // 将"当前"数据的时间戳传入 (Time Injection)
        let (new_bid, new_ask) = strategy.calculate_quotes(tick.mid_price, tick.timestamp);

        // --- C. 记录/更新挂单 ---
        // 这里的报价将在下一轮循环(T+1)中进行撮合判断
        my_last_bid = new_bid;
        my_last_ask = new_ask;

        // --- D. 周期性打印状态 (防止刷屏) ---
        if index % 10 == 0 {
            println!("{} | {} / {} | {} / {} | {:.2}", 
                tick.timestamp, 
                tick.best_bid, tick.best_ask, 
                new_bid, new_ask, 
                strategy.realized_inventory
            );
        }
    }

    // 4. 最终统计
    println!("-------------------------------------------------------------");
    println!("🏁 回测结束");
    println!("📊 交易次数: {}", trade_count);
    println!("💰 最终持仓: {:.4}", strategy.realized_inventory);
    println!("💵 现金余额: {:.4}", strategy.current_cash_balance);
    // 粗略估算总权益 (Cash + InventoryValue)
    // 注意：这里需要最后一笔数据的 mid_price，简化起见假设为 0.5 或者取变量
    println!("💡 (注意: 准确PnL需要加上 最终持仓 * 最终价格)");
}