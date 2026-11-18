//! Timers on `setTimeout` and a clock on `Date.now()`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::pin::Pin;
use std::task::{Context, Poll};

use real_tokio::sync::oneshot;
use wasm_bindgen::prelude::*;

pub use std::time::Duration;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(callback: &JsValue, ms: f64) -> JsValue;
    #[wasm_bindgen(js_name = clearTimeout)]
    fn clear_timeout(id: &JsValue);
}

thread_local! {
    // Timer ids are JS values, which are not `Send`. Futures hold a key into
    // this table instead so they stay `Send` for `async_trait` callers.
    static TIMERS: RefCell<HashMap<u64, JsValue>> = RefCell::new(HashMap::new());
    static NEXT_TIMER: RefCell<u64> = const { RefCell::new(0) };
}

/// A point in time read from `Date.now()`. `std::time::Instant` panics on
/// wasm32-unknown-unknown.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Instant {
    ms: f64,
}

impl Eq for Instant {}

impl Ord for Instant {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.ms.total_cmp(&other.ms)
    }
}

impl std::hash::Hash for Instant {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ms.to_bits().hash(state)
    }
}

impl Instant {
    pub fn now() -> Self {
        Self {
            ms: js_sys::Date::now(),
        }
    }

    pub fn elapsed(&self) -> Duration {
        Self::now().saturating_duration_since(*self)
    }

    pub fn duration_since(&self, earlier: Instant) -> Duration {
        self.saturating_duration_since(earlier)
    }

    pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
        Duration::from_secs_f64(((self.ms - earlier.ms) / 1000.0).max(0.0))
    }

    pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
        (self.ms >= earlier.ms).then(|| self.saturating_duration_since(earlier))
    }

    pub fn checked_add(&self, duration: Duration) -> Option<Instant> {
        Some(*self + duration)
    }

    pub fn checked_sub(&self, duration: Duration) -> Option<Instant> {
        Some(*self - duration)
    }

    pub fn into_std(self) -> Instant {
        self
    }

    pub fn from_std(instant: Instant) -> Instant {
        instant
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, rhs: Duration) -> Instant {
        Instant {
            ms: self.ms + rhs.as_secs_f64() * 1000.0,
        }
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
        Instant {
            ms: self.ms - rhs.as_secs_f64() * 1000.0,
        }
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

/// Future returned by [`sleep`] and [`sleep_until`].
#[derive(Debug)]
pub struct Sleep {
    key: u64,
    deadline: Instant,
    fired: oneshot::Receiver<()>,
}

impl Sleep {
    fn arm(deadline: Instant) -> Self {
        let ms = (deadline.ms - js_sys::Date::now()).max(0.0);
        let (tx, fired) = oneshot::channel();
        let key = NEXT_TIMER.with(|next| {
            let mut next = next.borrow_mut();
            *next += 1;
            *next
        });
        let callback = Closure::once_into_js(move || {
            TIMERS.with(|timers| timers.borrow_mut().remove(&key));
            let _ = tx.send(());
        });
        // setTimeout takes a 32-bit delay; a longer one fires immediately.
        let id = set_timeout(&callback, ms.min(2_147_483_647.0));
        TIMERS.with(|timers| timers.borrow_mut().insert(key, id));
        Self {
            key,
            deadline,
            fired,
        }
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn is_elapsed(&self) -> bool {
        Instant::now() >= self.deadline
    }

    pub fn reset(mut self: Pin<&mut Self>, deadline: Instant) {
        *self = Sleep::arm(deadline);
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        // `try_with`: thread-local storage may already be gone at teardown.
        let _ = TIMERS.try_with(|timers| {
            if let Some(id) = timers.borrow_mut().remove(&self.key) {
                clear_timeout(&id);
            }
        });
    }
}

impl Future for Sleep {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.fired).poll(cx).map(|_| ())
    }
}

pub fn sleep(duration: Duration) -> Sleep {
    Sleep::arm(Instant::now() + duration)
}

pub fn sleep_until(deadline: Instant) -> Sleep {
    Sleep::arm(deadline)
}

pub mod error {
    /// The deadline of a [`timeout`](super::timeout) passed.
    #[derive(Debug, PartialEq, Eq)]
    pub struct Elapsed(pub(crate) ());

    impl std::fmt::Display for Elapsed {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("deadline has elapsed")
        }
    }

    impl std::error::Error for Elapsed {}
}

/// Future returned by [`timeout`] and [`timeout_at`].
pub struct Timeout<F> {
    future: Pin<Box<F>>,
    sleep: Sleep,
}

impl<F: Future> Future for Timeout<F> {
    type Output = Result<F::Output, error::Elapsed>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(value) = self.future.as_mut().poll(cx) {
            return Poll::Ready(Ok(value));
        }
        match Pin::new(&mut self.sleep).poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(error::Elapsed(()))),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub fn timeout<F: Future>(duration: Duration, future: F) -> Timeout<F> {
    timeout_at(Instant::now() + duration, future)
}

pub fn timeout_at<F: Future>(deadline: Instant, future: F) -> Timeout<F> {
    Timeout {
        future: Box::pin(future),
        sleep: Sleep::arm(deadline),
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MissedTickBehavior {
    #[default]
    Burst,
    Delay,
    Skip,
}

/// Ticks every `period`. The first tick completes immediately, as in tokio.
/// A late tick always delays the following ones, whatever the configured
/// [`MissedTickBehavior`].
#[derive(Debug)]
pub struct Interval {
    period: Duration,
    next: Instant,
    behavior: MissedTickBehavior,
}

impl Interval {
    pub async fn tick(&mut self) -> Instant {
        sleep_until(self.next).await;
        let fired = Instant::now();
        self.next = fired + self.period;
        fired
    }

    pub fn reset(&mut self) {
        self.next = Instant::now() + self.period;
    }

    pub fn reset_at(&mut self, deadline: Instant) {
        self.next = deadline;
    }

    pub fn reset_after(&mut self, after: Duration) {
        self.next = Instant::now() + after;
    }

    pub fn reset_immediately(&mut self) {
        self.next = Instant::now();
    }

    pub fn period(&self) -> Duration {
        self.period
    }

    pub fn missed_tick_behavior(&self) -> MissedTickBehavior {
        self.behavior
    }

    pub fn set_missed_tick_behavior(&mut self, behavior: MissedTickBehavior) {
        self.behavior = behavior;
    }
}

pub fn interval(period: Duration) -> Interval {
    interval_at(Instant::now(), period)
}

pub fn interval_at(start: Instant, period: Duration) -> Interval {
    Interval {
        period,
        next: start,
        behavior: MissedTickBehavior::default(),
    }
}
