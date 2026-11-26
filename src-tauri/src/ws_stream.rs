use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, warn};

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// High-throughput local loopback WebSocket hub that streams RGBA dirty rects,
/// server pointer shapes, and session status updates to the Tauri WebView with minimal latency.
#[derive(Clone)]
pub struct StreamHub {
    pub port: u16,
    pub token: String,
    tx: broadcast::Sender<Arc<Vec<u8>>>,
    input: Arc<Mutex<Option<InputSink>>>,
}

/// Receives raw input frames sent by the WebView over the same ordered loopback socket.
pub type InputSink = Arc<dyn Fn(&[u8]) + Send + Sync>;

#[derive(Debug, Serialize)]
pub struct SessionStatusPayload<'a> {
    pub session_id: &'a str,
    pub state: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'a str>,
    pub message: &'a str,
    pub width: u16,
    pub height: u16,
    pub rtt_ms: Option<u32>,
}

impl StreamHub {
    pub fn set_input_sink(&self, sink: InputSink) {
        if let Ok(mut g) = self.input.lock() {
            *g = Some(sink);
        }
    }

    pub async fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();

        let mut token_bytes = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut token_bytes);
        let token = token_bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();

        let (tx, _) = broadcast::channel::<Arc<Vec<u8>>>(256);
        let hub = Self {
            port,
            token: token.clone(),
            tx: tx.clone(),
            input: Arc::new(Mutex::new(None)),
        };
        let input_slot = hub.input.clone();

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        // Critical for interactive typing/IME latency: disable Nagle's algorithm
                        // on loopback TCP so small dirty rects never stall 40-200ms on Windows.
                        let _ = stream.set_nodelay(true);
                        let expected_token = token.clone();
                        let mut rx = tx.subscribe();
                        let input_slot = input_slot.clone();
                        tokio::spawn(async move {
                            let mut authorized = false;
                            let ws_res = tokio_tungstenite::accept_hdr_async(
                                stream,
                                |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                                 resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
                                    if let Some(q) = req.uri().query() {
                                        for pair in q.split('&') {
                                            if let Some(val) = pair.strip_prefix("token=") {
                                                if constant_time_eq(
                                                    val.as_bytes(),
                                                    expected_token.as_bytes(),
                                                ) {
                                                    authorized = true;
                                                }
                                            }
                                        }
                                    }
                                    if authorized {
                                        Ok(resp)
                                    } else {
                                        let forbidden = tokio_tungstenite::tungstenite::http::Response::builder()
                                            .status(403)
                                            .body(Some("Forbidden".to_string()))
                                            .unwrap_or_else(|_| {
                                                tokio_tungstenite::tungstenite::http::Response::new(Some(
                                                    "Forbidden".to_string(),
                                                ))
                                            });
                                        Err(forbidden)
                                    }
                                },
                            )
                            .await;

                            let ws = match ws_res {
                                Ok(ws) => ws,
                                Err(e) => {
                                    debug!(%addr, %e, "WS handshake rejected or failed");
                                    return;
                                }
                            };

                            let (mut sink, mut source) = ws.split();
                            let read_task = tokio::spawn(async move {
                                while let Some(Ok(msg)) = source.next().await {
                                    if let Message::Binary(data) = msg {
                                        let sink = input_slot.lock().ok().and_then(|g| g.clone());
                                        if let Some(sink) = sink {
                                            sink(&data);
                                        }
                                    }
                                }
                            });

                            loop {
                                match rx.recv().await {
                                    Ok(frame) => {
                                        if sink.send(Message::Binary((*frame).clone().into())).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(broadcast::error::RecvError::Lagged(n)) => {
                                        warn!(skipped = n, "Frontend stream lagged, continuing");
                                    }
                                    Err(broadcast::error::RecvError::Closed) => break,
                                }
                            }
                            read_task.abort();
                        });
                    }
                    Err(e) => {
                        warn!(%e, "WS accept error");
                    }
                }
            }
        });

        Ok(hub)
    }

    #[cfg(test)]
    pub fn subscribe_raw(&self) -> broadcast::Receiver<Arc<Vec<u8>>> {
        self.tx.subscribe()
    }

    /// Type 0x01: RGBA dirty rectangle update
    #[allow(dead_code)]
    pub fn send_rgba_rect(
        &self,
        session_id: &str,
        fb_w: u16,
        fb_h: u16,
        left: u16,
        top: u16,
        rw: u16,
        rh: u16,
        rgba: &[u8],
    ) {
        let sid = session_id.as_bytes();
        let mut buf = Vec::with_capacity(1 + 2 + sid.len() + 12 + rgba.len());
        buf.push(0x01);
        buf.extend_from_slice(&(sid.len() as u16).to_le_bytes());
        buf.extend_from_slice(sid);
        buf.extend_from_slice(&fb_w.to_le_bytes());
        buf.extend_from_slice(&fb_h.to_le_bytes());
        buf.extend_from_slice(&left.to_le_bytes());
        buf.extend_from_slice(&top.to_le_bytes());
        buf.extend_from_slice(&rw.to_le_bytes());
        buf.extend_from_slice(&rh.to_le_bytes());
        buf.extend_from_slice(rgba);
        let _ = self.tx.send(Arc::new(buf));
    }

    /// Type 0x02: Atomic multi-rectangle RGBA frame batch (eliminates scanline tearing)
    pub fn send_rgba_batch(
        &self,
        session_id: &str,
        fb_w: u16,
        fb_h: u16,
        rects: &[(u16, u16, u16, u16, Vec<u8>)],
    ) {
        if rects.is_empty() {
            return;
        }
        let sid = session_id.as_bytes();
        let payload_bytes: usize = rects.iter().map(|r| 8 + r.4.len()).sum();
        let mut buf = Vec::with_capacity(1 + 2 + sid.len() + 6 + payload_bytes);
        buf.push(0x02);
        buf.extend_from_slice(&(sid.len() as u16).to_le_bytes());
        buf.extend_from_slice(sid);
        buf.extend_from_slice(&fb_w.to_le_bytes());
        buf.extend_from_slice(&fb_h.to_le_bytes());
        buf.extend_from_slice(&(rects.len().min(u16::MAX as usize) as u16).to_le_bytes());
        for (left, top, rw, rh, rgba) in rects.iter().take(u16::MAX as usize) {
            buf.extend_from_slice(&left.to_le_bytes());
            buf.extend_from_slice(&top.to_le_bytes());
            buf.extend_from_slice(&rw.to_le_bytes());
            buf.extend_from_slice(&rh.to_le_bytes());
            buf.extend_from_slice(rgba);
        }
        let _ = self.tx.send(Arc::new(buf));
    }

    /// Type 0x03: Session status JSON packet
    pub fn send_status(&self, payload: &SessionStatusPayload<'_>) {
        if let Ok(json) = serde_json::to_vec(payload) {
            let mut buf = Vec::with_capacity(1 + json.len());
            buf.push(0x03);
            buf.extend_from_slice(&json);
            let _ = self.tx.send(Arc::new(buf));
        }
    }

    /// Type 0x04 / subkind 0x00: Reset remote pointer to default system arrow
    pub fn send_pointer_default(&self, session_id: &str) {
        let sid = session_id.as_bytes();
        let mut buf = Vec::with_capacity(1 + 2 + sid.len() + 1);
        buf.push(0x04);
        buf.extend_from_slice(&(sid.len() as u16).to_le_bytes());
        buf.extend_from_slice(sid);
        buf.push(0x00);
        let _ = self.tx.send(Arc::new(buf));
    }

    /// Type 0x04 / subkind 0x01: Hide remote pointer (e.g. while typing in text field / video fullscreen)
    pub fn send_pointer_hidden(&self, session_id: &str) {
        let sid = session_id.as_bytes();
        let mut buf = Vec::with_capacity(1 + 2 + sid.len() + 1);
        buf.push(0x04);
        buf.extend_from_slice(&(sid.len() as u16).to_le_bytes());
        buf.extend_from_slice(sid);
        buf.push(0x01);
        let _ = self.tx.send(Arc::new(buf));
    }

    /// Type 0x04 / subkind 0x02: Custom server RGBA32 pointer bitmap (hand, I-beam, resize, busy, app cursors)
    pub fn send_pointer_bitmap(
        &self,
        session_id: &str,
        width: u16,
        height: u16,
        hotspot_x: u16,
        hotspot_y: u16,
        rgba: &[u8],
    ) {
        let expected = usize::from(width) * usize::from(height) * 4;
        if width == 0 || height == 0 || rgba.len() < expected {
            self.send_pointer_hidden(session_id);
            return;
        }
        let sid = session_id.as_bytes();
        let mut buf = Vec::with_capacity(1 + 2 + sid.len() + 1 + 8 + expected);
        buf.push(0x04);
        buf.extend_from_slice(&(sid.len() as u16).to_le_bytes());
        buf.extend_from_slice(sid);
        buf.push(0x02);
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());
        buf.extend_from_slice(&hotspot_x.to_le_bytes());
        buf.extend_from_slice(&hotspot_y.to_le_bytes());
        buf.extend_from_slice(&rgba[..expected]);
        let _ = self.tx.send(Arc::new(buf));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_packets_encode_default_hidden_and_bitmap() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let hub = rt.block_on(StreamHub::start()).unwrap();
        let mut rx = hub.subscribe_raw();

        hub.send_pointer_default("s1");
        let f0 = rx.try_recv().unwrap();
        assert_eq!(&*f0, &[0x04, 2, 0, b's', b'1', 0x00]);

        hub.send_pointer_hidden("s1");
        let f1 = rx.try_recv().unwrap();
        assert_eq!(&*f1, &[0x04, 2, 0, b's', b'1', 0x01]);

        // Invisible 0x0 bitmap should fall back to subkind 0x01 (hidden)
        hub.send_pointer_bitmap("s1", 0, 0, 0, 0, &[]);
        let f_inv = rx.try_recv().unwrap();
        assert_eq!(&*f_inv, &[0x04, 2, 0, b's', b'1', 0x01]);

        // 2x1 RGBA bitmap with hotspot (1, 0)
        let rgba = [10u8, 20, 30, 255, 40, 50, 60, 128];
        hub.send_pointer_bitmap("s1", 2, 1, 1, 0, &rgba);
        let f2 = rx.try_recv().unwrap();
        assert_eq!(f2[0], 0x04);
        assert_eq!(u16::from_le_bytes([f2[1], f2[2]]), 2);
        assert_eq!(&f2[3..5], b"s1");
        assert_eq!(f2[5], 0x02);
        assert_eq!(u16::from_le_bytes([f2[6], f2[7]]), 2); // w
        assert_eq!(u16::from_le_bytes([f2[8], f2[9]]), 1); // h
        assert_eq!(u16::from_le_bytes([f2[10], f2[11]]), 1); // hot_x
        assert_eq!(u16::from_le_bytes([f2[12], f2[13]]), 0); // hot_y
        assert_eq!(&f2[14..], &rgba);
    }
}
