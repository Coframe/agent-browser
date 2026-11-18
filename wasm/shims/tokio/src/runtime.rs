//! There is no tokio runtime on this platform: `agent-browser-host` drives
//! every future. A [`Runtime`] is a handle to it.

use std::future::Future;
use std::io;

#[derive(Debug)]
pub struct Runtime(Handle);

impl Runtime {
    pub fn new() -> io::Result<Self> {
        Ok(Runtime(Handle(())))
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        agent_browser_host::block_on(future)
    }

    pub fn handle(&self) -> &Handle {
        &self.0
    }

    pub fn spawn<F>(&self, future: F) -> crate::task::JoinHandle<F::Output>
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        crate::task::spawn(future)
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

    pub fn enable_io(&mut self) -> &mut Self {
        self
    }

    pub fn enable_time(&mut self) -> &mut Self {
        self
    }

    pub fn worker_threads(&mut self, _threads: usize) -> &mut Self {
        self
    }

    pub fn build(&mut self) -> io::Result<Runtime> {
        Runtime::new()
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

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        agent_browser_host::block_on(future)
    }
}
