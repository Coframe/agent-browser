//! Runtime abstraction over task spawning and timers.
//!
//! Native targets delegate to tokio. The wasm32 implementation drives the
//! same APIs from the JavaScript event loop so the daemon core can run in
//! environments like Cloudflare Workers.

pub use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    pub use std::time::Instant;
    pub use tokio::task::{spawn_blocking, JoinHandle};
    pub use tokio::time::{interval, sleep, timeout, MissedTickBehavior};

    pub async fn sleep_until(deadline: Instant) {
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    }

    pub async fn timeout_at<F: std::future::Future>(
        deadline: Instant,
        future: F,
    ) -> Result<F::Output, tokio::time::error::Elapsed> {
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), future).await
    }

    pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        tokio::spawn(future)
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::Duration;
    use futures_util::future::{AbortHandle, Abortable};
    use std::future::Future;
    use std::ops::{Add, AddAssign, Sub, SubAssign};

    /// Monotonic-enough instant backed by `Date.now()`.
    #[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
    pub struct Instant {
        millis: f64,
    }

    impl Instant {
        pub fn now() -> Self {
            Instant {
                millis: js_sys::Date::now(),
            }
        }

        pub fn elapsed(&self) -> Duration {
            Instant::now().saturating_duration_since(*self)
        }

        pub fn duration_since(&self, earlier: Instant) -> Duration {
            self.saturating_duration_since(earlier)
        }

        pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
            let delta = self.millis - earlier.millis;
            if delta <= 0.0 {
                Duration::ZERO
            } else {
                Duration::from_secs_f64(delta / 1000.0)
            }
        }

        pub fn checked_add(&self, duration: Duration) -> Option<Instant> {
            Some(Instant {
                millis: self.millis + duration.as_secs_f64() * 1000.0,
            })
        }

        pub fn checked_sub(&self, duration: Duration) -> Option<Instant> {
            Some(Instant {
                millis: self.millis - duration.as_secs_f64() * 1000.0,
            })
        }

        pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
            if self.millis >= earlier.millis {
                Some(self.saturating_duration_since(earlier))
            } else {
                None
            }
        }
    }

    impl Add<Duration> for Instant {
        type Output = Instant;
        fn add(self, rhs: Duration) -> Instant {
            self.checked_add(rhs).unwrap()
        }
    }

    impl AddAssign<Duration> for Instant {
        fn add_assign(&mut self, rhs: Duration) {
            *self = *self + rhs;
        }
    }

    impl Sub<Duration> for Instant {
        type Output = Instant;
        fn sub(self, rhs: Duration) -> Instant {
            self.checked_sub(rhs).unwrap()
        }
    }

    impl SubAssign<Duration> for Instant {
        fn sub_assign(&mut self, rhs: Duration) {
            *self = *self - rhs;
        }
    }

    impl Sub<Instant> for Instant {
        type Output = Duration;
        fn sub(self, rhs: Instant) -> Duration {
            self.saturating_duration_since(rhs)
        }
    }

    pub async fn sleep(duration: Duration) {
        let millis = duration.as_millis().min(i32::MAX as u128) as i32;
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let global = js_sys::global();
            let set_timeout = js_sys::Reflect::get(&global, &"setTimeout".into())
                .expect("setTimeout not available");
            let set_timeout: js_sys::Function = set_timeout.into();
            set_timeout
                .call2(&global, &resolve, &millis.into())
                .expect("setTimeout call failed");
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }

    pub async fn sleep_until(deadline: Instant) {
        let now = Instant::now();
        if deadline > now {
            sleep(deadline.saturating_duration_since(now)).await;
        }
    }

    /// Error returned when a timeout elapses, mirroring `tokio::time::error::Elapsed`.
    #[derive(Debug)]
    pub struct Elapsed;

    impl std::fmt::Display for Elapsed {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "deadline has elapsed")
        }
    }

    impl std::error::Error for Elapsed {}

    pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
        use futures_util::future::{select, Either};
        let sleep_fut = Box::pin(sleep(duration));
        let future = Box::pin(future);
        match select(future, sleep_fut).await {
            Either::Left((value, _)) => Ok(value),
            Either::Right(((), _)) => Err(Elapsed),
        }
    }

    pub async fn timeout_at<F: Future>(deadline: Instant, future: F) -> Result<F::Output, Elapsed> {
        let now = Instant::now();
        timeout(deadline.saturating_duration_since(now), future).await
    }

    #[derive(Debug, Clone, Copy)]
    pub enum MissedTickBehavior {
        Burst,
        Delay,
        Skip,
    }

    pub struct Interval {
        period: Duration,
        next: Instant,
    }

    impl Interval {
        pub fn set_missed_tick_behavior(&mut self, _behavior: MissedTickBehavior) {}

        pub async fn tick(&mut self) -> Instant {
            sleep_until(self.next).await;
            let tick = self.next;
            self.next = Instant::now() + self.period;
            tick
        }
    }

    pub fn interval(period: Duration) -> Interval {
        Interval {
            period,
            next: Instant::now(),
        }
    }

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
            if self.cancelled {
                write!(f, "task was cancelled")
            } else {
                write!(f, "task failed")
            }
        }
    }

    impl std::error::Error for JoinError {}

    /// Handle to a task spawned on the JavaScript event loop.
    pub struct JoinHandle<T> {
        receiver: tokio::sync::oneshot::Receiver<T>,
        abort_handle: AbortHandle,
    }

    impl<T> JoinHandle<T> {
        pub fn abort(&self) {
            self.abort_handle.abort();
        }

        pub fn is_finished(&self) -> bool {
            self.abort_handle.is_aborted()
        }
    }

    impl<T> Future for JoinHandle<T> {
        type Output = Result<T, JoinError>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            let this = self.get_mut();
            std::pin::Pin::new(&mut this.receiver)
                .poll(cx)
                .map(|result| result.map_err(|_| JoinError { cancelled: true }))
        }
    }

    pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        let task = Abortable::new(future, abort_registration);
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(output) = task.await {
                let _ = sender.send(output);
            }
        });
        JoinHandle {
            receiver,
            abort_handle,
        }
    }

    pub fn spawn_blocking<F, T>(f: F) -> JoinHandle<T>
    where
        F: FnOnce() -> T + 'static,
        T: 'static,
    {
        spawn(async move { f() })
    }
}

pub use imp::*;
