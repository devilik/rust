use zmq::{Context, Socket, PUB, SUB};
use crate::{OrderBookUpdate, TradeSignal};
use bincode;
use std::sync::{Arc, Mutex};

// 使用 Arc<Mutex<Socket>> 允许在多线程中克隆和共享（简单起见）
// 在极高性能场景下，建议每个线程一个 Socket 或使用 flume/crossbeam channel
#[derive(Clone)]
pub struct ZmqPublisher {
    socket: Arc<Mutex<Socket>>,
}

impl ZmqPublisher {
    pub fn new(endpoint: &str) -> Self {
        let ctx = Context::new();
        let socket = ctx.socket(PUB).unwrap();
        socket.set_sndhwm(10_000).unwrap(); // 高水位防止爆内存
        socket.set_linger(0).unwrap();      // 立即关闭不等待
        socket.bind(endpoint).unwrap();
        
        Self { socket: Arc::new(Mutex::new(socket)) }
    }

    /// 广播行情数据 (Topic: MD)
    pub fn send_book_update(&self, update: &OrderBookUpdate) {
        let encoded = bincode::serialize(update).unwrap();
        let sock = self.socket.lock().unwrap();
        sock.send_multipart(&["MD".as_bytes(), &encoded], 0).unwrap();
    }

    /// 广播交易信号 (Topic: SG) - 修复了 Engine 调用报错的问题
    pub fn send_signal(&self, signal: &TradeSignal) {
        let encoded = bincode::serialize(signal).unwrap();
        let sock = self.socket.lock().unwrap();
        sock.send_multipart(&["SG".as_bytes(), &encoded], 0).unwrap();
    }
}

pub struct ZmqSubscriber {
    socket: Socket,
}

impl ZmqSubscriber {
    pub fn new(endpoint: &str, topic: &str) -> Self {
        let ctx = Context::new();
        let socket = ctx.socket(SUB).unwrap();
        socket.connect(endpoint).unwrap();
        socket.set_subscribe(topic.as_bytes()).unwrap();
        Self { socket }
    }

    /// 接收行情数据
    pub fn recv_book_update(&self) -> Option<OrderBookUpdate> {
        let msg = self.socket.recv_multipart(0).ok()?;
        if msg.len() < 2 { return None; }
        bincode::deserialize(&msg[1]).ok()
    }

    /// 通用接收方法 - 修复了 Execution Loop 无法接收信号的问题
    pub fn recv_raw_bytes(&self) -> Option<Vec<u8>> {
        let msg = self.socket.recv_multipart(0).ok()?;
        if msg.len() < 2 { return None; }
        Some(msg[1].to_vec())
    }
}