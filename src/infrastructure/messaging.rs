use zmq::{Context, Socket, PUB, SUB};
use crate::{OrderBookUpdate, TradeSignal};
use bincode;

pub struct ZmqPublisher {
    socket: Socket,
}

impl ZmqPublisher {
    pub fn new(endpoint: &str) -> Self {
        let ctx = Context::new();
        let socket = ctx.socket(PUB).unwrap();
        // 关键优化：设置高水位 (HWM) 防止内存爆掉，设置 Linger=0 防止关机卡死
        socket.set_sndhwm(10_000).unwrap();
        socket.set_linger(0).unwrap();
        socket.bind(endpoint).unwrap();
        
        Self { socket }
    }

    /// 极速发送：序列化 -> 推送
    pub fn send_book_update(&self, update: &OrderBookUpdate) {
        // bincode 序列化比 json 快 10-50 倍
        let encoded: Vec<u8> = bincode::serialize(update).unwrap();
        // 发送 Topic (例如 "MARKET_DATA") 和 Payload
        self.socket.send_multipart(&["MD".as_bytes(), &encoded], 0).unwrap();
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

    pub fn recv_book_update(&self) -> Option<OrderBookUpdate> {
        // 非阻塞接收
        let msg = self.socket.recv_multipart(0).ok()?;
        if msg.len() < 2 { return None; }
        
        // 0是Topic, 1是Data
        let data = &msg[1];
        bincode::deserialize(data).ok()
    }
}