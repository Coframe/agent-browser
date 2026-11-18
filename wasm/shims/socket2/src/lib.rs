//! The subset of the socket2 API that agent-browser uses, for wasm32 hosts.
//! There is no socket to tune or shut down: the JS host owns the connection,
//! so every call succeeds and does nothing.

use std::io;
use std::marker::PhantomData;
use std::net::Shutdown;
use std::time::Duration;

#[derive(Debug)]
pub struct Socket(());

impl Socket {
    pub fn shutdown(&self, _how: Shutdown) -> io::Result<()> {
        Ok(())
    }

    pub fn try_clone(&self) -> io::Result<Socket> {
        Ok(Socket(()))
    }

    pub fn set_tcp_keepalive(&self, _keepalive: &TcpKeepalive) -> io::Result<()> {
        Ok(())
    }

    pub fn set_nodelay(&self, _nodelay: bool) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub struct SockRef<'s> {
    socket: Socket,
    _borrow: PhantomData<&'s ()>,
}

impl<'s, S> From<&'s S> for SockRef<'s> {
    fn from(_socket: &'s S) -> Self {
        SockRef {
            socket: Socket(()),
            _borrow: PhantomData,
        }
    }
}

impl std::ops::Deref for SockRef<'_> {
    type Target = Socket;
    fn deref(&self) -> &Socket {
        &self.socket
    }
}

#[derive(Clone, Debug, Default)]
pub struct TcpKeepalive(());

impl TcpKeepalive {
    pub fn new() -> Self {
        Self(())
    }

    pub fn with_time(self, _time: Duration) -> Self {
        self
    }

    pub fn with_interval(self, _interval: Duration) -> Self {
        self
    }

    pub fn with_retries(self, _retries: u32) -> Self {
        self
    }
}
