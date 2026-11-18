//! Tasks on the JS microtask queue through `wasm_bindgen_futures`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::future::{AbortHandle as FutAbortHandle, Abortable};
use real_tokio::sync::oneshot;

#[derive(Debug)]
pub struct JoinError {
    cancelled: bool,
}

impl JoinError {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn is_panic(&self) -> bool {
        !self.cancelled
    }
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.cancelled {
            "task was cancelled"
        } else {
            "task failed"
        })
    }
}

impl std::error::Error for JoinError {}

#[derive(Clone, Debug)]
pub struct AbortHandle {
    abort: FutAbortHandle,
    finished: Arc<AtomicBool>,
}

impl AbortHandle {
    pub fn abort(&self) {
        self.abort.abort();
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub struct JoinHandle<T> {
    result: oneshot::Receiver<T>,
    handle: AbortHandle,
}

impl<T> JoinHandle<T> {
    pub fn abort(&self) {
        self.handle.abort();
    }

    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    pub fn abort_handle(&self) -> AbortHandle {
        self.handle.clone()
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let cancelled = self.handle.abort.is_aborted();
        Pin::new(&mut self.result)
            .poll(cx)
            .map(|result| result.map_err(|_| JoinError { cancelled }))
    }
}

/// Runs `future` on the JS event loop. Unlike tokio, the future does not
/// have to be `Send`: a wasm32 host has one thread.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    let (tx, result) = oneshot::channel();
    let (abort, registration) = FutAbortHandle::new_pair();
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    wasm_bindgen_futures::spawn_local(async move {
        if let Ok(output) = Abortable::new(future, registration).await {
            let _ = tx.send(output);
        }
        done.store(true, Ordering::SeqCst);
    });
    JoinHandle {
        result,
        handle: AbortHandle { abort, finished },
    }
}

/// Runs `f` inline: there is no second thread to block.
pub fn spawn_blocking<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + 'static,
    T: 'static,
{
    spawn(async move { f() })
}

pub async fn yield_now() {
    struct Yield(bool);
    impl Future for Yield {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                return Poll::Ready(());
            }
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
    Yield(false).await
}
