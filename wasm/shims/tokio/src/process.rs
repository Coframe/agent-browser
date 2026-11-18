//! Child processes do not exist on this platform. `spawn` and friends return
//! `io::ErrorKind::Unsupported`.

use std::ffi::OsStr;
use std::io;
use std::pin::Pin;
use std::process::{ExitStatus, Output, Stdio};
use std::task::{Context, Poll};

use real_tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::unsupported;

#[derive(Debug, Default)]
pub struct Command(());

impl Command {
    pub fn new<S: AsRef<OsStr>>(_program: S) -> Self {
        Self(())
    }

    pub fn arg<S: AsRef<OsStr>>(&mut self, _arg: S) -> &mut Self {
        self
    }

    pub fn args<I, S>(&mut self, _args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self
    }

    pub fn env<K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, _key: K, _val: V) -> &mut Self {
        self
    }

    pub fn env_remove<K: AsRef<OsStr>>(&mut self, _key: K) -> &mut Self {
        self
    }

    pub fn current_dir<P: AsRef<std::path::Path>>(&mut self, _dir: P) -> &mut Self {
        self
    }

    pub fn stdin<T: Into<Stdio>>(&mut self, _cfg: T) -> &mut Self {
        self
    }

    pub fn stdout<T: Into<Stdio>>(&mut self, _cfg: T) -> &mut Self {
        self
    }

    pub fn stderr<T: Into<Stdio>>(&mut self, _cfg: T) -> &mut Self {
        self
    }

    pub fn kill_on_drop(&mut self, _kill_on_drop: bool) -> &mut Self {
        self
    }

    pub fn spawn(&mut self) -> io::Result<Child> {
        Err(unsupported("spawning a process"))
    }

    pub async fn output(&mut self) -> io::Result<Output> {
        Err(unsupported("spawning a process"))
    }

    pub async fn status(&mut self) -> io::Result<ExitStatus> {
        Err(unsupported("spawning a process"))
    }
}

macro_rules! stub_pipe {
    ($name:ident) => {
        #[derive(Debug)]
        pub struct $name(());

        impl AsyncRead for $name {
            fn poll_read(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Poll::Ready(Err(unsupported("process io")))
            }
        }

        impl AsyncWrite for $name {
            fn poll_write(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &[u8],
            ) -> Poll<io::Result<usize>> {
                Poll::Ready(Err(unsupported("process io")))
            }

            fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Err(unsupported("process io")))
            }

            fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Err(unsupported("process io")))
            }
        }
    };
}

stub_pipe!(ChildStdin);
stub_pipe!(ChildStdout);
stub_pipe!(ChildStderr);

#[derive(Debug)]
pub struct Child {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}

impl Child {
    pub fn id(&self) -> Option<u32> {
        None
    }

    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        Err(unsupported("waiting for a process"))
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Err(unsupported("waiting for a process"))
    }

    pub async fn wait_with_output(self) -> io::Result<Output> {
        Err(unsupported("waiting for a process"))
    }

    pub async fn kill(&mut self) -> io::Result<()> {
        Err(unsupported("killing a process"))
    }

    pub fn start_kill(&mut self) -> io::Result<()> {
        Err(unsupported("killing a process"))
    }
}
