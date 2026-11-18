//! The subset of the tokio-tungstenite API that agent-browser uses, for
//! wasm32 hosts.
//!
//! A wasm32 host cannot open a TCP socket, so the host opens the WebSocket
//! and this crate adapts it to the `WebSocketStream` that upstream code
//! expects. The host registers a connector once with [`set_connector`]:
//!
//! ```text
//! connector(url, headers, onEvent) -> Promise<{ send(text), close() }>
//! onEvent("message", text) | onEvent("close", reason?) | onEvent("error", message)
//! ```
//!
//! Message, error, request and config types are tungstenite's own. Accepting
//! connections is not possible here and returns an error.

use std::cell::RefCell;
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::{Sink, Stream};
use tokio::sync::mpsc;
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::client::Response;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Error, Message};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

pub use tungstenite;

thread_local! {
    // JS values are not `Send`. Streams hold a key into this table so they
    // can sit behind `Arc<Mutex<..>>` and inside `async_trait` futures.
    static SOCKETS: RefCell<HashMap<u64, JsValue>> = RefCell::new(HashMap::new());
    static NEXT_SOCKET: RefCell<u64> = const { RefCell::new(0) };
    static CONNECTOR: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
}

/// Registers the function that opens WebSockets for this isolate. See the
/// crate docs for its contract.
pub fn set_connector(connector: js_sys::Function) {
    CONNECTOR.with(|slot| *slot.borrow_mut() = Some(connector));
}

fn io_error(message: impl Into<String>) -> Error {
    Error::Io(std::io::Error::other(message.into()))
}

fn js_message(value: &JsValue) -> String {
    value
        .dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| value.as_string())
        .unwrap_or_else(|| format!("{value:?}"))
}

/// TLS is the host's concern on this platform. The type exists so callers
/// that pass `Option<Connector>` compile.
#[derive(Clone, Debug)]
pub enum Connector {
    Plain,
}

#[derive(Debug)]
pub struct TlsStream<S>(S, ());

impl<S> TlsStream<S> {
    pub fn get_ref(&self) -> (&S, &()) {
        (&self.0, &self.1)
    }
}

#[derive(Debug)]
pub enum MaybeTlsStream<S> {
    Plain(S),
    Rustls(TlsStream<S>),
}

#[derive(Debug)]
pub struct WebSocketStream<S> {
    inner: S,
    key: u64,
    events: mpsc::UnboundedReceiver<Result<Message, Error>>,
    closed: bool,
}

impl<S> WebSocketStream<S> {
    pub fn get_ref(&self) -> &S {
        &self.inner
    }

    pub fn get_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    fn call(&self, method: &str, arg: Option<&str>) -> Result<(), Error> {
        SOCKETS.with(|sockets| {
            let sockets = sockets.borrow();
            let socket = sockets.get(&self.key).ok_or(Error::AlreadyClosed)?;
            let function = js_sys::Reflect::get(socket, &JsValue::from_str(method))
                .ok()
                .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                .ok_or_else(|| io_error(format!("WebSocket transport has no {method}()")))?;
            let result = match arg {
                Some(arg) => function.call1(socket, &JsValue::from_str(arg)),
                None => function.call0(socket),
            };
            result.map(|_| ()).map_err(|e| io_error(js_message(&e)))
        })
    }

    pub async fn close(
        &mut self,
        _frame: Option<tungstenite::protocol::CloseFrame<'_>>,
    ) -> Result<(), Error> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let result = self.call("close", None);
        SOCKETS.with(|sockets| sockets.borrow_mut().remove(&self.key));
        result
    }
}

impl<S> Drop for WebSocketStream<S> {
    fn drop(&mut self) {
        if !self.closed {
            self.closed = true;
            // `try_with`: thread-local storage may already be gone at teardown.
            let _ = SOCKETS.try_with(|sockets| {
                if let Some(socket) = sockets.borrow_mut().remove(&self.key) {
                    if let Some(close) = js_sys::Reflect::get(&socket, &JsValue::from_str("close"))
                        .ok()
                        .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                    {
                        let _ = close.call0(&socket);
                    }
                }
            });
        }
    }
}

impl<S: Unpin> Stream for WebSocketStream<S> {
    type Item = Result<Message, Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.events.poll_recv(cx)
    }
}

impl<S: Unpin> Sink<Message> for WebSocketStream<S> {
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Error> {
        match item {
            Message::Text(text) => self.call("send", Some(text.as_str())),
            Message::Binary(data) => match String::from_utf8(data) {
                Ok(text) => self.call("send", Some(&text)),
                Err(_) => Err(io_error("binary frames are not supported on this platform")),
            },
            // The host keeps the connection alive; there is no ping frame to send.
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
                if self.closed {
                    Err(Error::AlreadyClosed)
                } else {
                    Ok(())
                }
            }
            Message::Close(_) => self.shutdown(),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(self.shutdown())
    }
}

type ClientStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub async fn connect_async<R>(request: R) -> Result<(ClientStream, Response), Error>
where
    R: IntoClientRequest + Unpin,
{
    connect_async_tls_with_config(request, None, false, None).await
}

pub async fn connect_async_with_config<R>(
    request: R,
    config: Option<WebSocketConfig>,
    disable_nagle: bool,
) -> Result<(ClientStream, Response), Error>
where
    R: IntoClientRequest + Unpin,
{
    connect_async_tls_with_config(request, config, disable_nagle, None).await
}

pub async fn connect_async_tls_with_config<R>(
    request: R,
    _config: Option<WebSocketConfig>,
    _disable_nagle: bool,
    _connector: Option<Connector>,
) -> Result<(ClientStream, Response), Error>
where
    R: IntoClientRequest + Unpin,
{
    let request = request.into_client_request()?;
    // JS values are `!Send` and callers await this inside `Send` futures.
    // A wasm32 host runs on one thread, which `SendWrapper` checks.
    let key = send_wrapper::SendWrapper::new(open_socket(request)).await?;
    let (key, events) = key;
    let stream = WebSocketStream {
        inner: MaybeTlsStream::Plain(tokio::net::TcpStream::placeholder()),
        key,
        events,
        closed: false,
    };
    Ok((stream, Response::new(None)))
}

type Events = mpsc::UnboundedReceiver<Result<Message, Error>>;

async fn open_socket(
    request: tungstenite::handshake::client::Request,
) -> Result<(u64, Events), Error> {
    let url = request.uri().to_string();
    let headers = js_sys::Array::new();
    for (name, value) in request.headers() {
        if let Ok(value) = value.to_str() {
            headers.push(&js_sys::Array::of2(
                &JsValue::from_str(name.as_str()),
                &JsValue::from_str(value),
            ));
        }
    }

    let connector = CONNECTOR
        .with(|slot| slot.borrow().clone())
        .ok_or_else(|| io_error("no WebSocket connector is registered on this platform"))?;

    let (tx, events) = mpsc::unbounded_channel();
    let on_event =
        Closure::<dyn FnMut(JsValue, JsValue)>::new(move |kind: JsValue, data: JsValue| {
            let event = match kind.as_string().as_deref() {
                Some("message") => match data.as_string() {
                    Some(text) => Ok(Message::Text(text)),
                    None => return,
                },
                Some("close") => Ok(Message::Close(data.as_string().map(|reason| {
                    tungstenite::protocol::CloseFrame {
                        code: tungstenite::protocol::frame::coding::CloseCode::Normal,
                        reason: reason.into(),
                    }
                }))),
                _ => Err(io_error(js_message(&data))),
            };
            let _ = tx.send(event);
        })
        .into_js_value();

    let promise = connector
        .call3(
            &JsValue::NULL,
            &JsValue::from_str(&url),
            &headers,
            &on_event,
        )
        .map_err(|e| io_error(js_message(&e)))?;
    let socket = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&promise))
        .await
        .map_err(|e| io_error(js_message(&e)))?;

    let key = NEXT_SOCKET.with(|next| {
        let mut next = next.borrow_mut();
        *next += 1;
        *next
    });
    SOCKETS.with(|sockets| sockets.borrow_mut().insert(key, socket));
    Ok((key, events))
}

/// Accepting connections needs a listening socket, which this platform lacks.
pub async fn accept_async<S>(_stream: S) -> Result<WebSocketStream<S>, Error> {
    Err(io_error(
        "accepting WebSocket connections is not available on this platform",
    ))
}

pub async fn accept_hdr_async<S, C>(_stream: S, _callback: C) -> Result<WebSocketStream<S>, Error> {
    Err(io_error(
        "accepting WebSocket connections is not available on this platform",
    ))
}
