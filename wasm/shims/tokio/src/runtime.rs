//! There is no tokio runtime on this platform: the host executor drives
//! every future. Building one returns `io::ErrorKind::Unsupported`.

use std::future::Future;
use std::io;

use crate::unsupported;

#[derive(Debug)]
pub struct Runtime(());

impl Runtime {
    pub fn new() -> io::Result<Self> {
        Err(unsupported("a tokio runtime"))
    }

    pub fn block_on<F: Future>(&self, _future: F) -> F::Output {
        unreachable!("a Runtime cannot be built on this platform")
    }

    pub fn handle(&self) -> &Handle {
        &Handle(())
    }
}

#[derive(Debug, Default)]
pub struct Builder(());

impl Builder {
    pub fn new_current_thread() -> Self {
        Self(())
    }

    pub fn new_multi_thread() -> Self {
        Self(())
    }

    pub fn enable_all(&mut self) -> &mut Self {
        self
    }

    pub fn worker_threads(&mut self, _threads: usize) -> &mut Self {
        self
    }

    pub fn build(&mut self) -> io::Result<Runtime> {
        Err(unsupported("a tokio runtime"))
    }
}

#[derive(Debug)]
pub struct TryCurrentError(());

impl std::fmt::Display for TryCurrentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("there is no tokio runtime on this platform")
    }
}

impl std::error::Error for TryCurrentError {}

/// Stands in for the runtime handle. `spawn` uses the host executor.
#[derive(Clone, Debug)]
pub struct Handle(());

impl Handle {
    pub fn current() -> Self {
        Self(())
    }

    pub fn try_current() -> Result<Self, TryCurrentError> {
        Ok(Self(()))
    }

    pub fn spawn<F>(&self, future: F) -> crate::task::JoinHandle<F::Output>
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        crate::task::spawn(future)
    }

    pub fn block_on<F: Future>(&self, _future: F) -> F::Output {
        panic!("block_on is not available on this platform")
    }
}
