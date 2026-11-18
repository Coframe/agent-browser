//! Loopback TCP between instances of this module, carried by the host.
//! Anything else returns `io::ErrorKind::Unsupported`: the module cannot
//! reach the network through sockets.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use real_tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::unsupported;

macro_rules! stub_stream {
    ($name:ident) => {
        #[derive(Debug)]
        pub struct $name(pub(crate) ());

        impl AsyncRead for $name {
            fn poll_read(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Poll::Ready(Err(unsupported("socket io")))
            }
        }

        impl AsyncWrite for $name {
            fn poll_write(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &[u8],
            ) -> Poll<io::Result<usize>> {
                Poll::Ready(Err(unsupported("socket io")))
            }

            fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Err(unsupported("socket io")))
            }

            fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Err(unsupported("socket io")))
            }
        }
    };
}

stub_stream!(UnixStream);

/// A loopback connection to another instance of this module, carried by
/// the host. Only the accepting side is async.
#[derive(Debug)]
pub struct TcpStream {
    conn: Option<agent_browser_host::Conn>,
    unread: Vec<u8>,
}

impl TcpStream {
    /// Stands in for the socket under a WebSocket that the host owns.
    #[doc(hidden)]
    pub fn placeholder() -> Self {
        TcpStream {
            conn: None,
            unread: Vec::new(),
        }
    }

    /// Outbound connections block on this platform. Use
    /// `agent_browser_host::tcp_connect`.
    pub async fn connect<A>(_addr: A) -> io::Result<Self> {
        Err(unsupported("an async tcp connect"))
    }

    pub fn from_std(_stream: std::net::TcpStream) -> io::Result<Self> {
        Err(unsupported("tcp"))
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    pub fn set_nodelay(&self, _nodelay: bool) -> io::Result<()> {
        Ok(())
    }

    pub async fn peek(&self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(unsupported("tcp peek"))
    }

    pub fn into_split(self) -> (tcp::OwnedReadHalf, tcp::OwnedWriteHalf) {
        (tcp::OwnedReadHalf(()), tcp::OwnedWriteHalf(()))
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.unread.is_empty() {
            let Some(conn) = self.conn.as_mut() else {
                return Poll::Ready(Ok(()));
            };
            match conn.poll_read(cx) {
                Poll::Ready(Some(data)) => self.unread = data,
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
        let take = self.unread.len().min(buf.remaining());
        buf.put_slice(&self.unread[..take]);
        self.unread.drain(..take);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(match &self.conn {
            Some(conn) => conn.write(buf),
            None => Err(io::ErrorKind::NotConnected.into()),
        })
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.conn = None;
        Poll::Ready(Ok(()))
    }
}

pub mod tcp {
    use super::*;
    stub_stream!(OwnedReadHalf);
    stub_stream!(OwnedWriteHalf);
}

impl UnixStream {
    pub async fn connect<P>(_path: P) -> io::Result<Self> {
        Err(unsupported("unix sockets"))
    }
}

/// A loopback listener that other instances of this module connect to
/// through the host. Only the port of the bind address is used.
#[derive(Debug)]
pub struct TcpListener(std::sync::Mutex<agent_browser_host::TcpListener>);

impl TcpListener {
    pub async fn bind<A: std::net::ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let port = addr
            .to_socket_addrs()?
            .next()
            .map(|addr| addr.port())
            .ok_or(io::ErrorKind::InvalidInput)?;
        agent_browser_host::tcp_listen(port)
            .map(|listener| TcpListener(std::sync::Mutex::new(listener)))
    }

    pub fn from_std(_listener: std::net::TcpListener) -> io::Result<Self> {
        Err(unsupported("tcp"))
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let conn = std::future::poll_fn(|cx| self.0.lock().unwrap().poll_accept(cx)).await?;
        let stream = TcpStream {
            conn: Some(conn),
            unread: Vec::new(),
        };
        Ok((stream, SocketAddr::from(([127, 0, 0, 1], 0))))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::from((
            [127, 0, 0, 1],
            self.0.lock().unwrap().port(),
        )))
    }
}

#[derive(Debug)]
pub struct UnixListener(());

impl UnixListener {
    pub fn bind<P>(_path: P) -> io::Result<Self> {
        Err(unsupported("unix sockets"))
    }

    pub async fn accept(&self) -> io::Result<(UnixStream, ())> {
        Err(unsupported("unix sockets"))
    }
}

pub async fn lookup_host<T>(_host: T) -> io::Result<std::vec::IntoIter<SocketAddr>> {
    Err(unsupported("dns lookup"))
}
