//! The subset of the tokio-tungstenite API that agent-browser uses, for
//! wasm32-wasip1.
//!
//! The module cannot open a TCP socket, so the JS host opens the WebSocket
//! through `agent-browser-host` and this crate adapts it to the
//! `WebSocketStream` that upstream code expects. Message, error, request and
//! config types are tungstenite's own. Accepting connections is not possible
//! here and returns an error.

use std::pin::Pin;
use std::task::{Context, Poll};

use agent_browser_host::{WebSocket, WsEvent};
use futures_util::{Sink, Stream, StreamExt};
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::client::Response;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{Error, Message};

pub use tungstenite;

fn io_error(message: impl Into<String>) -> Error {
    Error::Io(std::io::Error::other(message.into()))
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
    socket: Option<WebSocket>,
    events: futures_channel::mpsc::UnboundedReceiver<WsEvent>,
}

impl<S> WebSocketStream<S> {
    pub fn get_ref(&self) -> &S {
        &self.inner
    }

    pub fn get_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    pub async fn close(
        &mut self,
        _frame: Option<tungstenite::protocol::CloseFrame<'_>>,
    ) -> Result<(), Error> {
        self.socket = None;
        Ok(())
    }

    fn send_text(&self, text: &str) -> Result<(), Error> {
        match &self.socket {
            Some(socket) => socket.send(text).map_err(io_error),
            None => Err(Error::AlreadyClosed),
        }
    }
}

impl<S: Unpin> Stream for WebSocketStream<S> {
    type Item = Result<Message, Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            return match self.events.poll_next_unpin(cx) {
                Poll::Ready(Some(WsEvent::Open)) => continue,
                Poll::Ready(Some(WsEvent::Message(text))) => Poll::Ready(Some(Ok(Message::Text(text)))),
                Poll::Ready(Some(WsEvent::Close(reason))) => {
                    Poll::Ready(Some(Ok(Message::Close(reason.map(|reason| {
                        tungstenite::protocol::CloseFrame {
                            code: tungstenite::protocol::frame::coding::CloseCode::Normal,
                            reason: reason.into(),
                        }
                    })))))
                }
                Poll::Ready(Some(WsEvent::Error(message))) => Poll::Ready(Some(Err(io_error(message)))),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            };
        }
    }
}

impl<S: Unpin> Sink<Message> for WebSocketStream<S> {
    type Error = Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Error> {
        match item {
            Message::Text(text) => self.send_text(&text),
            Message::Binary(data) => match String::from_utf8(data) {
                Ok(text) => self.send_text(&text),
                Err(_) => Err(io_error("binary frames are not supported on this platform")),
            },
            // The host keeps the connection alive; there is no ping frame to send.
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => match self.socket {
                Some(_) => Ok(()),
                None => Err(Error::AlreadyClosed),
            },
            Message::Close(_) => {
                self.socket = None;
                Ok(())
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.socket = None;
        Poll::Ready(Ok(()))
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
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.as_str().to_string(), value.to_str().ok()?.to_string())))
        .collect();
    let (socket, mut events) = agent_browser_host::ws_open(&request.uri().to_string(), &headers);
    match events.next().await {
        Some(WsEvent::Open) => {}
        Some(WsEvent::Error(message)) => return Err(io_error(message)),
        Some(WsEvent::Close(reason)) => {
            return Err(io_error(reason.unwrap_or_else(|| "connection closed".to_string())))
        }
        Some(WsEvent::Message(_)) | None => return Err(io_error("connection failed")),
    }
    let stream = WebSocketStream {
        inner: MaybeTlsStream::Plain(tokio::net::TcpStream::placeholder()),
        socket: Some(socket),
        events,
    };
    Ok((stream, Response::new(None)))
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
