use zmq::{Context, Socket, PUB, SUB};
use crate::core::{OrderBookUpdate, TradeSignal, InventoryUpdate};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct ZmqPublisher {
    socket: Arc<Mutex<Socket>>,
}

impl ZmqPublisher {
    pub fn new(endpoint: &str) -> Self {
        let ctx = Context::new();
        let socket = ctx.socket(PUB).unwrap();
        
        // [关键设置]
        socket.set_sndhwm(10_000).unwrap(); // 高水位：防止内存溢出
        socket.set_linger(0).unwrap();      // 立即关闭：防止进程卡死
        socket.bind(endpoint).unwrap();
        
        Self { socket: Arc::new(Mutex::new(socket)) }
    }

    pub fn send_book_update(&self, update: &OrderBookUpdate) {
        let encoded = bincode::serialize(update).unwrap();
        self.socket.lock().unwrap().send_multipart(&["MD".as_bytes(), &encoded], 0).unwrap();
    }

    pub fn send_signal(&self, signal: &TradeSignal) {
        let encoded = bincode::serialize(signal).unwrap();
        self.socket.lock().unwrap().send_multipart(&["SG".as_bytes(), &encoded], 0).unwrap();
    }

    pub fn send_inventory_update(&self, update: &InventoryUpdate) {
        let encoded = bincode::serialize(update).unwrap();
        self.socket.lock().unwrap().send_multipart(&["IV".as_bytes(), &encoded], 0).unwrap();
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

    // 通用接收：返回原始字节供反序列化
    pub fn recv_raw_bytes(&self) -> Option<Vec<u8>> {
        let msg = self.socket.recv_multipart(0).ok()?;
        if msg.len() < 2 { return None; }
        Some(msg[1].to_vec())
    }
}