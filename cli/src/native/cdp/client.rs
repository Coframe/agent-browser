use std::collections::HashMap;
use std::io::Write;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use futures_util::SinkExt;
use futures_util::{Stream, StreamExt};
use serde_json::Value;
use tokio::sync::{broadcast, oneshot, Mutex};
#[cfg(not(target_arch = "wasm32"))]
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
#[cfg(not(target_arch = "wasm32"))]
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
#[cfg(not(target_arch = "wasm32"))]
use tokio_tungstenite::tungstenite::Message;

use super::types::{CdpCommand, CdpEvent, CdpMessage};

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<CdpMessage>>>>;

/// Incoming message from a CDP transport, decoupled from any specific
/// WebSocket implementation.
#[derive(Debug)]
pub enum TransportEvent {
    Text(String),
    Close(Option<String>),
    Error(String),
}

/// Outgoing half of a CDP transport. Implemented by tokio-tungstenite on
/// native targets; other runtimes (e.g. Cloudflare Workers) supply their own.
#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
pub trait CdpTransportSink: Send + Sync {
    async fn send_text(&self, text: String) -> Result<(), String>;

    /// Send a protocol-level ping frame. Transports without ping support
    /// treat this as a no-op.
    async fn send_ping(&self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
pub trait CdpTransportSink {
    async fn send_text(&self, text: String) -> Result<(), String>;

    /// Send a protocol-level ping frame. Transports without ping support
    /// treat this as a no-op.
    async fn send_ping(&self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub type TransportEventStream = Pin<Box<dyn Stream<Item = TransportEvent> + Send>>;
#[cfg(target_arch = "wasm32")]
pub type TransportEventStream = Pin<Box<dyn Stream<Item = TransportEvent>>>;

/// Interval between WebSocket ping frames sent to keep the connection alive
/// through intermediate proxies (reverse proxies, load balancers, service meshes).
const WS_KEEPALIVE_INTERVAL_SECS: u64 = 30;

/// Raw incoming CDP message (text) broadcast to all subscribers.
/// Used by the inspect proxy to forward responses and events to DevTools.
#[derive(Debug, Clone)]
pub struct RawCdpMessage {
    pub text: String,
    pub session_id: Option<String>,
}

pub struct CdpClient {
    ws_tx: Arc<dyn CdpTransportSink>,
    next_id: AtomicU64,
    pending: PendingMap,
    event_tx: broadcast::Sender<CdpEvent>,
    raw_tx: broadcast::Sender<RawCdpMessage>,
    _reader_handle: crate::rt::JoinHandle<()>,
    _keepalive_handle: crate::rt::JoinHandle<()>,
}

/// Removes a pending entry if `send_command` is cancelled mid-await (e.g. an
/// outer timeout on the liveness probe), so a command whose response never
/// comes can't leak until the connection closes (#1528). Normal exits disarm
/// it via `done`.
struct PendingGuard {
    pending: PendingMap,
    id: u64,
    done: bool,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let pending = self.pending.clone();
        let id = self.id;
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                pending.lock().await.remove(&id);
            });
        }
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            pending.lock().await.remove(&id);
        });
    }
}

/// tokio-tungstenite implementation of the transport sink used on native targets.
#[cfg(not(target_arch = "wasm32"))]
struct TungsteniteSink {
    tx: Mutex<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    >,
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl CdpTransportSink for TungsteniteSink {
    async fn send_text(&self, text: String) -> Result<(), String> {
        let mut tx = self.tx.lock().await;
        tx.send(Message::Text(text))
            .await
            .map_err(|e| e.to_string())
    }

    async fn send_ping(&self) -> Result<(), String> {
        let mut tx = self.tx.lock().await;
        tx.send(Message::Ping(Vec::new()))
            .await
            .map_err(|e| e.to_string())
    }
}

impl CdpClient {
    #[cfg(target_arch = "wasm32")]
    pub async fn connect(_url: &str) -> Result<Self, String> {
        Err(
            "direct WebSocket connect is not supported on this platform; \
             construct the client with CdpClient::from_transport"
                .to_string(),
        )
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn connect_with_headers(
        _url: &str,
        _headers: Option<Vec<(String, String)>>,
    ) -> Result<Self, String> {
        Err(
            "direct WebSocket connect is not supported on this platform; \
             construct the client with CdpClient::from_transport"
                .to_string(),
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn connect(url: &str) -> Result<Self, String> {
        Self::connect_with_headers(url, None).await
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn connect_with_headers(
        url: &str,
        headers: Option<Vec<(String, String)>>,
    ) -> Result<Self, String> {
        let mut request = url
            .into_client_request()
            .map_err(|e| format!("Invalid WebSocket URL: {}", e))?;

        if let Some(hdrs) = headers {
            let req_headers = request.headers_mut();
            for (key, value) in hdrs {
                if let (Ok(name), Ok(val)) = (
                    key.parse::<tokio_tungstenite::tungstenite::http::header::HeaderName>(),
                    value.parse::<tokio_tungstenite::tungstenite::http::header::HeaderValue>(),
                ) {
                    req_headers.insert(name, val);
                }
            }
        }

        let ws_config = WebSocketConfig {
            max_message_size: None,
            max_frame_size: None,
            ..Default::default()
        };

        let (ws_stream, _) =
            tokio_tungstenite::connect_async_with_config(request, Some(ws_config), false)
                .await
                .map_err(|e| format!("CDP WebSocket connect failed: {}", e))?;

        enable_tcp_keepalive(ws_stream.get_ref());

        let (ws_tx, ws_rx) = ws_stream.split();

        // Accept both Text and Binary frames — remote CDP proxies
        // (e.g. Browserless) may send responses as Binary frames.
        let events: TransportEventStream = Box::pin(ws_rx.filter_map(|msg| async move {
            match msg {
                Ok(Message::Text(text)) => Some(TransportEvent::Text(text)),
                Ok(Message::Binary(data)) => String::from_utf8(data).ok().map(TransportEvent::Text),
                Ok(Message::Close(frame)) => Some(TransportEvent::Close(
                    frame
                        .as_ref()
                        .map(|f| format!("code={}, reason={}", f.code, f.reason)),
                )),
                Ok(_) => None,
                Err(e) => Some(TransportEvent::Error(e.to_string())),
            }
        }));

        Ok(Self::from_transport(
            Arc::new(TungsteniteSink {
                tx: Mutex::new(ws_tx),
            }),
            events,
        ))
    }

    /// Build a client on top of an already-established transport. This is the
    /// runtime-injection seam: any WebSocket-like transport that can deliver
    /// text frames works, regardless of the underlying platform.
    pub fn from_transport(
        sink: Arc<dyn CdpTransportSink>,
        mut events: TransportEventStream,
    ) -> Self {
        let ws_tx = sink;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, _) = broadcast::channel(4096);
        let (raw_tx, _) = broadcast::channel(4096);

        let pending_clone = pending.clone();
        let event_tx_clone = event_tx.clone();
        let raw_tx_clone = raw_tx.clone();

        // Notify used to stop the keepalive task when the reader loop exits.
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);

        let reader_handle = crate::rt::spawn(async move {
            while let Some(event) = events.next().await {
                let msg = match event {
                    TransportEvent::Text(text) => text,
                    TransportEvent::Close(reason) => {
                        if std::env::var("AGENT_BROWSER_DEBUG").is_ok() {
                            let reason = reason.unwrap_or_else(|| "no frame".to_string());
                            let _ =
                                writeln!(std::io::stderr(), "[cdp] WebSocket Close: {}", reason);
                        }
                        break;
                    }
                    TransportEvent::Error(e) => {
                        if std::env::var("AGENT_BROWSER_DEBUG").is_ok() {
                            let _ = writeln!(std::io::stderr(), "[cdp] WebSocket Error: {}", e);
                        }
                        break;
                    }
                };

                // Broadcast raw message for inspect proxy subscribers before typed parse,
                // so messages with negative IDs (used by the inspect proxy) are still delivered.
                if raw_tx_clone.receiver_count() > 0 {
                    let session_id = serde_json::from_str::<serde_json::Value>(&msg)
                        .ok()
                        .and_then(|v| v.get("sessionId")?.as_str().map(String::from));
                    let _ = raw_tx_clone.send(RawCdpMessage {
                        text: msg.clone(),
                        session_id,
                    });
                }

                let parsed: CdpMessage = match serde_json::from_str(&msg) {
                    Ok(m) => m,
                    // Expected for inspect proxy messages with negative IDs
                    // (CdpMessage.id is u64); handled via raw broadcast above.
                    Err(_) => continue,
                };

                if let Some(id) = parsed.id {
                    // Response to a command
                    let mut pending = pending_clone.lock().await;
                    if let Some(tx) = pending.remove(&id) {
                        let _ = tx.send(parsed);
                    }
                } else if let Some(ref method) = parsed.method {
                    // Event
                    let event = CdpEvent {
                        method: method.clone(),
                        params: parsed.params.clone().unwrap_or(Value::Null),
                        session_id: parsed.session_id.clone(),
                    };
                    let _ = event_tx_clone.send(event);
                }
            }

            // Reader loop exited (connection closed or error). Drop all pending
            // command senders so callers get an immediate channel-closed error
            // instead of waiting for the 30-second timeout.
            pending_clone.lock().await.clear();

            // Stop the keepalive task — the connection is gone.
            let _ = cancel_tx.send(true);
        });

        // Spawn a keepalive task that sends WebSocket Ping frames at a regular
        // interval. This prevents intermediate proxies (Envoy, nginx, OpenResty,
        // cloud load balancers) from closing idle WebSocket connections. If the
        // send fails, the connection is dead and we stop pinging.
        let keepalive_tx = ws_tx.clone();
        let keepalive_handle = crate::rt::spawn(async move {
            let interval = std::time::Duration::from_secs(WS_KEEPALIVE_INTERVAL_SECS);
            loop {
                tokio::select! {
                    _ = crate::rt::sleep(interval) => {}
                    _ = cancel_rx.changed() => break,
                }
                if keepalive_tx.send_ping().await.is_err() {
                    break;
                }
            }
        });

        Self {
            ws_tx,
            next_id: AtomicU64::new(1),
            pending,
            event_tx,
            raw_tx,
            _reader_handle: reader_handle,
            _keepalive_handle: keepalive_handle,
        }
    }

    pub async fn send_command(
        &self,
        method: &str,
        params: Option<Value>,
        session_id: Option<&str>,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let cmd = CdpCommand {
            id,
            method: method.to_string(),
            params,
            session_id: session_id.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        };

        let json = serde_json::to_string(&cmd)
            .map_err(|e| format!("Failed to serialize CDP command: {}", e))?;

        let (tx, rx) = oneshot::channel();

        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        // Cleans up the pending entry if this future is cancelled mid-await (#1528).
        let mut guard = PendingGuard {
            pending: self.pending.clone(),
            id,
            done: false,
        };

        self.ws_tx
            .send_text(json)
            .await
            .map_err(|e| format!("Failed to send CDP command: {}", e))?;

        let response = match crate::rt::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(resp)) => {
                guard.done = true;
                resp
            }
            Ok(Err(_)) => {
                guard.done = true;
                return Err("CDP response channel closed".to_string());
            }
            Err(_) => {
                guard.done = true;
                self.pending.lock().await.remove(&id);
                return Err(format!("CDP command timed out: {}", method));
            }
        };

        if let Some(error) = response.error {
            return Err(format!("CDP error ({}): {}", method, error));
        }

        Ok(response.result.unwrap_or(Value::Null))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.event_tx.subscribe()
    }

    /// Subscribe to all raw incoming CDP messages (responses + events).
    /// Used by the inspect proxy to forward traffic to the DevTools frontend.
    pub fn subscribe_raw(&self) -> broadcast::Receiver<RawCdpMessage> {
        self.raw_tx.subscribe()
    }

    /// Create a lightweight handle for the inspect WebSocket proxy.
    /// Contains only what's needed to forward messages bidirectionally.
    pub fn inspect_handle(&self) -> InspectProxyHandle {
        InspectProxyHandle {
            ws_tx: self.ws_tx.clone(),
            raw_tx: self.raw_tx.clone(),
        }
    }

    pub async fn send_command_typed<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        session_id: Option<&str>,
    ) -> Result<R, String> {
        let params_value = serde_json::to_value(params)
            .map_err(|e| format!("Failed to serialize params: {}", e))?;
        let result = self
            .send_command(method, Some(params_value), session_id)
            .await?;
        serde_json::from_value(result)
            .map_err(|e| format!("Failed to deserialize CDP response for {}: {}", method, e))
    }

    pub async fn send_command_no_params(
        &self,
        method: &str,
        session_id: Option<&str>,
    ) -> Result<Value, String> {
        self.send_command(method, None, session_id).await
    }

    /// Send a CDP command without waiting for its response.
    ///
    /// This is useful for best-effort commands where Chrome may not emit a
    /// response for every target session, but the command still needs to be
    /// written before the caller can continue processing events.
    pub async fn send_command_no_wait(
        &self,
        method: &str,
        params: Option<Value>,
        session_id: Option<&str>,
    ) -> Result<(), String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let cmd = CdpCommand {
            id,
            method: method.to_string(),
            params,
            session_id: session_id.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        };

        let json = serde_json::to_string(&cmd)
            .map_err(|e| format!("Failed to serialize CDP command: {}", e))?;

        self.ws_tx
            .send_text(json)
            .await
            .map_err(|e| format!("Failed to send CDP command: {}", e))
    }

    /// Send raw JSON through the WebSocket without tracking a response.
    /// Used by the inspect proxy to forward DevTools frontend messages.
    pub async fn send_raw(&self, json: String) -> Result<(), String> {
        self.ws_tx
            .send_text(json)
            .await
            .map_err(|e| format!("Failed to send raw CDP message: {}", e))
    }

    /// Test-only: count of in-flight commands still awaiting a response, so a
    /// test can assert a cancelled command left no orphaned entry (#1528).
    #[cfg(test)]
    pub(crate) async fn pending_len(&self) -> usize {
        self.pending.lock().await.len()
    }
}

/// Lightweight handle for the inspect WebSocket proxy, holding only
/// the cloneable parts of CdpClient needed for bidirectional message forwarding.
pub struct InspectProxyHandle {
    ws_tx: Arc<dyn CdpTransportSink>,
    raw_tx: broadcast::Sender<RawCdpMessage>,
}

impl InspectProxyHandle {
    pub async fn send_raw(&self, json: String) -> Result<(), String> {
        self.ws_tx
            .send_text(json)
            .await
            .map_err(|e| format!("Failed to send raw CDP message: {}", e))
    }

    pub fn subscribe_raw(&self) -> broadcast::Receiver<RawCdpMessage> {
        self.raw_tx.subscribe()
    }
}

/// Enable TCP SO_KEEPALIVE on the underlying socket of a WebSocket connection.
/// This is best-effort: failures are silently ignored since the WebSocket-level
/// Ping keepalive provides the primary connection liveness mechanism.
#[cfg(not(target_arch = "wasm32"))]
fn enable_tcp_keepalive(stream: &tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>) {
    let tcp_stream = match stream {
        tokio_tungstenite::MaybeTlsStream::Plain(s) => s,
        tokio_tungstenite::MaybeTlsStream::Rustls(s) => s.get_ref().0,
        _ => return,
    };

    // SockRef borrows the fd without taking ownership.
    let sock = socket2::SockRef::from(tcp_stream);
    let keepalive = socket2::TcpKeepalive::new().with_time(std::time::Duration::from_secs(30));

    // with_interval sets TCP_KEEPINTVL — the time between probes after the
    // first keepalive probe goes unanswered. Available on most platforms
    // (Linux, macOS, Windows, FreeBSD, etc.) but not OpenBSD or Haiku.
    #[cfg(not(any(target_os = "openbsd", target_os = "haiku")))]
    let keepalive = keepalive.with_interval(std::time::Duration::from_secs(10));

    let _ = sock.set_tcp_keepalive(&keepalive);
}
