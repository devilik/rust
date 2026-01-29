// File: src/bin/backtest.rs

use rust_decimal::Decimal;
use rust_decimal::prelude::{ToPrimitive, FromPrimitive};
use rust_decimal_macros::dec;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::str::FromStr;
use enterprise_market_maker::model::as_logic::{OpinionGridStrategy, StrategyConfig};
use enterprise_market_maker::core::{OrderBookUpdate, Exchange};
use enterprise_market_maker::config::load_config; // 复用配置加载逻辑
use enterprise_market_maker::infrastructure::messaging::ZmqSubscriber;
// 定义 CSV 数据结构 (根据你实际的数据格式调整)
struct BacktestTick {
    timestamp: i64,
    poly_price: Decimal,    // Polymarket 价格
    opinion_price: Decimal, // Opinion 真实成交价 (用于撮合)
}

fn main() {
    println!("⏳ 初始化回测引擎 (Advanced Mode)...");

    // 1. 配置
    let config = StrategyConfig {
        risk_aversion_gamma: 0.05,
        liquidity_k: 5000.0,
        min_spread_bps: 50,
        tick_size: 0.01,
        max_inventory_usd: 2000.0,
        default_order_size_usd: 10.0,
        vol_window_size: 20,
        maturity_timestamp_ms: 1767139200000, 
        terminal_dumping_factor: 10.0,
        closing_window_seconds: 3600,
        pricing_source: "opinion".to_string(), // [测试点] 设为 "opinion" 测试自定价
    };

    let mut strategy = OpinionGridStrategy::new(config.clone(), None);

    // 2. 读取数据
    // 假设 CSV 格式: timestamp, poly_price, opinion_trade_price
    let file = File::open("backtest_data.csv").expect("❌ 请准备 backtest_data.csv: timestamp,poly_price,opinion_price");
    let reader = BufReader::new(file);

    // 统计变量
    let mut my_bid = dec!(0);
    let mut my_ask = dec!(0);
    let mut trade_log: Vec<f64> = Vec::new(); // 记录每笔 PnL
    let mut equity_curve: Vec<f64> = Vec::new();
    let mut max_equity = 0.0;
    let mut max_drawdown = 0.0;
    let mut last_mid = 0.0;

    println!("-------------------------------------------------------------------------------");
    println!("Ts | Anchor($) | Mkt($) | Qt(B/A) | Inv | Cash | PnL");
    println!("-------------------------------------------------------------------------------");

    for (index, line) in reader.lines().enumerate() {
        let line = line.unwrap();
        if index == 0 { continue; } // Header

        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 3 { continue; }

        let tick = BacktestTick {
            timestamp: parts[0].trim().parse().unwrap_or(0),
            poly_price: Decimal::from_str(parts[1].trim()).unwrap_or(dec!(0)),
            opinion_price: Decimal::from_str(parts[2].trim()).unwrap_or(dec!(0)),
        };
        
        let mkt_price_f64 = tick.opinion_price.to_f64().unwrap();
        last_mid = mkt_price_f64;

        // --- A. 撮合逻辑 (Matching) ---
        // 规则：如果不依赖 L2 订单簿回测，通常假设：
        // 1. 如果 Opinion 真实成交价 <= 我的 Bid，说明有人砸盘，我的买单成交
        // 2. 如果 Opinion 真实成交价 >= 我的 Ask，说明有人吃单，我的卖单成交
        
        let mut executed = false;
        let fill_size_usd = config.default_order_size_usd;

        // [Buy Fill Check]
        if !my_bid.is_zero() && tick.opinion_price <= my_bid {
            let shares = fill_size_usd / my_bid.to_f64().unwrap();
            strategy.on_fill_confirmed(shares, -fill_size_usd);
            executed = true;
            // println!("   ✅ BUY  @ {} (Mkt: {})", my_bid, tick.opinion_price);
        }
        // [Sell Fill Check]
        else if !my_ask.is_zero() && tick.opinion_price >= my_ask {
            let shares = fill_size_usd / my_ask.to_f64().unwrap();
            strategy.on_fill_confirmed(-shares, fill_size_usd);
            executed = true;
            // println!("   ✅ SELL @ {} (Mkt: {})", my_ask, tick.opinion_price);
        }

        if executed {
             // 简单的 PnL 记录 (Cash + Share Value)
             let equity = strategy.current_cash_balance + (strategy.realized_inventory * mkt_price_f64);
             trade_log.push(equity);
             
             // 更新回撤
             if equity > max_equity { max_equity = equity; }
             let dd = max_equity - equity;
             if dd > max_drawdown { max_drawdown = dd; }
        }

        // --- B. 策略计算 ---
        
        // 1. 确定锚定价格
        let anchor_price = if config.pricing_source == "opinion" {
            tick.opinion_price // 自定价模式：用 Opinion 自己的历史价格做均值/波动率计算
        } else {
            tick.poly_price // 跨市场模式：用 Poly 价格做锚点
        };

        // 2. 计算新报价
        let (new_bid, new_ask) = strategy.calculate_quotes(anchor_price, tick.timestamp);
        
        // 模拟 1 个 Tick 的延迟（当前计算出的报价，下一轮生效）
        my_bid = new_bid;
        my_ask = new_ask;

        if index % 100 == 0 {
             let equity = strategy.current_cash_balance + (strategy.realized_inventory * mkt_price_f64);
             println!("{} | {} | {} | {}/{} | {:.1} | {:.1} | {:.2}", 
                tick.timestamp, anchor_price, tick.opinion_price, new_bid, new_ask, 
                strategy.realized_inventory, strategy.current_cash_balance, equity);
        }
    }

    // --- C. 结果分析 ---
    let final_equity = strategy.current_cash_balance + (strategy.realized_inventory * last_mid);
    let total_return = final_equity; // 假设初始资金为 0 (Delta neutral 启动)
    
    println!("-------------------------------------------------------------------------------");
    println!("📊 回测报告 (Source: {})", config.pricing_source);
    println!("-------------------------------------------------------------------------------");
    println!("最终权益 (Equity) : ${:.2}", final_equity);
    println!("最大回撤 (Max DD) : ${:.2}", max_drawdown);
    println!("持仓库存 (End Inv): {:.2} shares", strategy.realized_inventory);
    println!("交易次数 (Trades) : {}", trade_log.len());
    
    // 简单的夏普比率估算 (假设无风险利率为0)
    if trade_log.len() > 1 {
        let mut returns = Vec::new();
        for i in 1..trade_log.len() {
            returns.push(trade_log[i] - trade_log[i-1]);
        }
        let mean_ret: f64 = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance: f64 = returns.iter().map(|&x| (x - mean_ret).powi(2)).sum::<f64>() / returns.len() as f64;
        let std_dev = variance.sqrt();
        if std_dev > 0.0 {
            // Annualized Sharpe (Roughly)
            let sharpe = mean_ret / std_dev * (trade_log.len() as f64).sqrt(); 
            println!("夏普比率 (Sharpe) : {:.4}", sharpe);
        }
    }
}