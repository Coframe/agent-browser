//! Sockets do not exist on this platform. The types are here so callers
//! compile; every constructor returns `io::ErrorKind::Unsupported`.

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

stub_stream!(TcpStream);
stub_stream!(UnixStream);

impl TcpStream {
    /// Stands in for the socket under a WebSocket that the JS host owns.
    #[doc(hidden)]
    pub fn placeholder() -> Self {
        Self(())
    }

    pub async fn connect<A>(_addr: A) -> io::Result<Self> {
        Err(unsupported("tcp"))
    }

    pub fn from_std(_stream: std::net::TcpStream) -> io::Result<Self> {
        Err(unsupported("tcp"))
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        Err(unsupported("tcp"))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Err(unsupported("tcp"))
    }

    pub fn set_nodelay(&self, _nodelay: bool) -> io::Result<()> {
        Err(unsupported("tcp"))
    }

    pub async fn peek(&self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(unsupported("tcp"))
    }

    pub fn into_split(self) -> (tcp::OwnedReadHalf, tcp::OwnedWriteHalf) {
        (tcp::OwnedReadHalf(()), tcp::OwnedWriteHalf(()))
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

#[derive(Debug)]
pub struct TcpListener(());

impl TcpListener {
    pub async fn bind<A>(_addr: A) -> io::Result<Self> {
        Err(unsupported("tcp"))
    }

    pub fn from_std(_listener: std::net::TcpListener) -> io::Result<Self> {
        Err(unsupported("tcp"))
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        Err(unsupported("tcp"))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Err(unsupported("tcp"))
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
