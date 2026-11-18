//! Timers on the host event loop and the WASI monotonic clock.

use std::future::Future;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::pin::Pin;
use std::task::{Context, Poll};

pub use std::time::Duration;

/// A point on the WASI monotonic clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(std::time::Instant);

impl Instant {
    pub fn now() -> Self {
        Self(std::time::Instant::now())
    }

    pub fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }

    pub fn duration_since(&self, earlier: Instant) -> Duration {
        self.0.saturating_duration_since(earlier.0)
    }

    pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
        self.0.saturating_duration_since(earlier.0)
    }

    pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
        self.0.checked_duration_since(earlier.0)
    }

    pub fn checked_add(&self, duration: Duration) -> Option<Instant> {
        self.0.checked_add(duration).map(Instant)
    }

    pub fn checked_sub(&self, duration: Duration) -> Option<Instant> {
        self.0.checked_sub(duration).map(Instant)
    }

    pub fn into_std(self) -> std::time::Instant {
        self.0
    }

    pub fn from_std(instant: std::time::Instant) -> Instant {
        Instant(instant)
    }
}

impl From<std::time::Instant> for Instant {
    fn from(instant: std::time::Instant) -> Self {
        Instant(instant)
    }
}

impl From<Instant> for std::time::Instant {
    fn from(instant: Instant) -> Self {
        instant.0
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, rhs: Duration) -> Instant {
        Instant(self.0 + rhs)
    }
}

impl AddAssign<Duration> for Instant {
    fn add_assign(&mut self, rhs: Duration) {
        self.0 += rhs;
    }
}

impl Sub<Duration> for Instant {
    type Output = Instant;
    fn sub(self, rhs: Duration) -> Instant {
        Instant(self.0 - rhs)
    }
}

impl SubAssign<Duration> for Instant {
    fn sub_assign(&mut self, rhs: Duration) {
        self.0 -= rhs;
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;
    fn sub(self, rhs: Instant) -> Duration {
        self.0.saturating_duration_since(rhs.0)
    }
}

/// Future returned by [`sleep`] and [`sleep_until`].
#[derive(Debug)]
pub struct Sleep {
    deadline: Instant,
    timer: agent_browser_host::Timer,
}

impl Sleep {
    fn arm(deadline: Instant) -> Self {
        let remaining = deadline.saturating_duration_since(Instant::now());
        Self {
            deadline,
            timer: agent_browser_host::timer(remaining.as_secs_f64() * 1000.0),
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

impl Future for Sleep {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.timer).poll(cx)
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
