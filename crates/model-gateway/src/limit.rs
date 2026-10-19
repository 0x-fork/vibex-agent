//! Per-account concurrency and request-rate limits.
//!
//! Vendors count requests per account, not per model or per Agent, so a limit
//! has to hold across everything that account serves. Two mechanisms cover it:
//!
//! - **A lane** caps how many requests are in flight at once. A request that
//!   finds every slot taken waits in a FIFO queue, bounded both in length and
//!   in how long it may wait, so a burst degrades into a wait and never into an
//!   unbounded backlog.
//! - **A minute window** caps how many requests may start in any rolling
//!   minute. It records the *scheduled* start time rather than the arrival
//!   time, so a request that waited still counts against the minute it
//!   actually leaves in.
//!
//! Both are best-effort guards, not billing: they bound what Vibex sends, and
//! say nothing about what the vendor counted.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::{GatewayError, GatewayResult};

/// How long one rate-limit window is.
const RPM_WINDOW: Duration = Duration::from_secs(60);
/// A little slack, because a request scheduled at the edge of a window arrives
/// at the vendor marginally later.
const RPM_SLACK: Duration = Duration::from_secs(1);

/// One account's in-flight budget and waiting requests.
#[derive(Debug, Default)]
struct Lane {
    limit: u32,
    busy: u32,
    /// Requests waiting for a slot, oldest first.
    queue: VecDeque<u64>,
    next_ticket: u64,
}

/// One account's rolling-minute budget.
#[derive(Debug, Default)]
struct MinuteWindow {
    /// The scheduled start times inside the current window, oldest first.
    starts: VecDeque<Instant>,
}

/// The service-wide limiter.
#[derive(Debug, Default)]
pub struct Limiter {
    lanes: Mutex<HashMap<String, Lane>>,
    minutes: Mutex<HashMap<String, MinuteWindow>>,
}

/// A held slot. Dropping it releases the slot, so a cancelled request cannot
/// leak capacity.
#[derive(Debug)]
pub struct Permit {
    limiter: LimiterHandle,
    who: String,
    released: bool,
}

#[derive(Clone, Debug)]
struct LimiterHandle(std::sync::Arc<Limiter>);

impl Limiter {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    /// Reserves a slot for `who`, waiting up to `wait` when the lane is full.
    ///
    /// Returns `429` rather than blocking forever: a request that cannot start
    /// within its own deadline should fail where the Agent can see it.
    pub async fn acquire(
        self: &std::sync::Arc<Self>,
        who: &str,
        max_concurrency: Option<u32>,
        queue_limit: Option<u32>,
        wait: Duration,
        now: Instant,
    ) -> GatewayResult<Permit> {
        let Some(limit) = max_concurrency.filter(|limit| *limit > 0) else {
            // No limit on this Key: the permit is a no-op, but it is still
            // returned so callers have one shape to release.
            return Ok(Permit {
                limiter: LimiterHandle(std::sync::Arc::clone(self)),
                who: who.to_string(),
                released: true,
            });
        };

        let deadline = now + wait;
        {
            let ticket = {
                let mut lanes = self.lanes.lock().map_err(|_| {
                    GatewayError::Internal("the gateway lane state is poisoned".to_string())
                })?;
                let lane = lanes.entry(who.to_string()).or_default();
                lane.limit = limit;
                if lane.busy < limit && lane.queue.is_empty() {
                    lane.busy += 1;
                    // A slot was free, so nothing was queued.
                    return Ok(Permit {
                        limiter: LimiterHandle(std::sync::Arc::clone(self)),
                        who: who.to_string(),
                        released: false,
                    });
                }
                if let Some(queue_limit) = queue_limit
                    && lane.queue.len() >= queue_limit as usize
                {
                    return Err(GatewayError::Upstream {
                        status: 429,
                        message:
                            "every request slot for this account is busy and its queue is full"
                                .to_string(),
                    });
                }
                lane.next_ticket += 1;
                let ticket = lane.next_ticket;
                lane.queue.push_back(ticket);
                ticket
            };

            // Wait to be granted, or give up at the deadline. The tiny poll
            // keeps this free of a per-lane condition variable while remaining
            // correct: a released permit is observed within one tick.
            let poll = Duration::from_millis(2);
            loop {
                let granted = {
                    let mut lanes = self.lanes.lock().map_err(|_| {
                        GatewayError::Internal("the gateway lane state is poisoned".to_string())
                    })?;
                    let lane = lanes.entry(who.to_string()).or_default();
                    if lane.queue.front() == Some(&ticket) && lane.busy < lane.limit {
                        lane.queue.pop_front();
                        lane.busy += 1;
                        true
                    } else {
                        false
                    }
                };
                if granted {
                    return Ok(Permit {
                        limiter: LimiterHandle(std::sync::Arc::clone(self)),
                        who: who.to_string(),
                        released: false,
                    });
                }
                if Instant::now() >= deadline {
                    let mut lanes = self.lanes.lock().map_err(|_| {
                        GatewayError::Internal("the gateway lane state is poisoned".to_string())
                    })?;
                    if let Some(lane) = lanes.get_mut(who) {
                        lane.queue.retain(|queued| *queued != ticket);
                    }
                    return Err(GatewayError::Upstream {
                        status: 429,
                        message: format!(
                            "waited {}s for a request slot and gave up",
                            wait.as_secs()
                        ),
                    });
                }
                tokio::time::sleep(poll).await;
            }
        }
    }

    /// Reserves one start in the rolling minute, waiting for room when needed.
    ///
    /// The scheduled start time is recorded, not the arrival time, so a request
    /// that queued still counts against the minute it leaves in.
    pub async fn reserve_minute(
        self: &std::sync::Arc<Self>,
        who: &str,
        max_rpm: Option<u32>,
        wait: Duration,
    ) -> GatewayResult<()> {
        let Some(limit) = max_rpm.filter(|limit| *limit > 0) else {
            return Ok(());
        };
        let deadline = Instant::now() + wait;
        loop {
            let at = {
                let mut minutes = self.minutes.lock().map_err(|_| {
                    GatewayError::Internal("the gateway rate state is poisoned".to_string())
                })?;
                let window = minutes.entry(who.to_string()).or_default();
                let now = Instant::now();
                let horizon = now.checked_sub(RPM_WINDOW + RPM_SLACK).unwrap_or(now);
                while window.starts.front().is_some_and(|start| *start <= horizon) {
                    window.starts.pop_front();
                }
                if window.starts.len() < limit as usize {
                    window.starts.push_back(now);
                    return Ok(());
                }
                // The earliest slot frees when the oldest start leaves the
                // window. The slack is added here rather than to the pruning
                // horizon so the vendor never sees one extra request.
                window
                    .starts
                    .front()
                    .copied()
                    .map(|oldest| oldest + RPM_WINDOW + RPM_SLACK)
            };
            let Some(at) = at else {
                return Ok(());
            };
            if at > deadline {
                return Err(GatewayError::Upstream {
                    status: 429,
                    message: format!(
                        "this account's {limit}-request minute is full; retry in {}s",
                        at.duration_since(Instant::now()).as_secs() + 1
                    ),
                });
            }
            tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
        }
    }

    fn release(&self, who: &str) {
        if let Ok(mut lanes) = self.lanes.lock()
            && let Some(lane) = lanes.get_mut(who)
        {
            lane.busy = lane.busy.saturating_sub(1);
            if lane.busy == 0 && lane.queue.is_empty() {
                lanes.remove(who);
            }
        }
    }

    /// How many requests are in flight for `who`, for tests and diagnostics.
    pub fn in_flight(&self, who: &str) -> u32 {
        self.lanes
            .lock()
            .ok()
            .and_then(|lanes| lanes.get(who).map(|lane| lane.busy))
            .unwrap_or(0)
    }

    /// How many requests are waiting for `who`.
    pub fn queued(&self, who: &str) -> usize {
        self.lanes
            .lock()
            .ok()
            .and_then(|lanes| lanes.get(who).map(|lane| lane.queue.len()))
            .unwrap_or(0)
    }
}

impl Permit {
    /// Releases the slot now rather than at drop, for a caller that wants to
    /// free capacity before it finishes bookkeeping.
    pub fn release(mut self) {
        if !self.released {
            self.limiter.0.release(&self.who);
            self.released = true;
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if !self.released {
            self.limiter.0.release(&self.who);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn wait() -> Duration {
        Duration::from_millis(500)
    }

    #[tokio::test]
    async fn a_slot_is_held_until_the_permit_drops() {
        let limiter = Limiter::new();
        let first = limiter
            .acquire("key", Some(1), None, wait(), Instant::now())
            .await
            .unwrap();
        assert_eq!(limiter.in_flight("key"), 1);
        drop(first);
        assert_eq!(limiter.in_flight("key"), 0);
    }

    #[tokio::test]
    async fn a_second_request_waits_then_takes_the_released_slot() {
        let limiter = Limiter::new();
        let first = limiter
            .acquire("key", Some(1), None, wait(), Instant::now())
            .await
            .unwrap();
        let cloned = Arc::clone(&limiter);
        let waiter = tokio::spawn(async move {
            cloned
                .acquire("key", Some(1), None, wait(), Instant::now())
                .await
        });
        // It is queued, not failed.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(limiter.queued("key"), 1);
        drop(first);
        let second = waiter.await.unwrap().unwrap();
        assert_eq!(limiter.in_flight("key"), 1);
        drop(second);
        assert_eq!(limiter.in_flight("key"), 0);
    }

    #[tokio::test]
    async fn a_full_queue_is_refused_rather_than_growing() {
        let limiter = Limiter::new();
        let _held = limiter
            .acquire("key", Some(1), Some(1), wait(), Instant::now())
            .await
            .unwrap();
        let cloned = Arc::clone(&limiter);
        let queued = tokio::spawn(async move {
            cloned
                .acquire("key", Some(1), Some(1), wait(), Instant::now())
                .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        // The queue is full, so a third request is refused immediately.
        let third = limiter
            .acquire("key", Some(1), Some(1), wait(), Instant::now())
            .await;
        let error = third.expect_err("a full queue must refuse");
        assert_eq!(error.status(), 429);
        drop(queued);
    }

    #[tokio::test]
    async fn waiting_out_the_deadline_is_a_429() {
        let limiter = Limiter::new();
        let _held = limiter
            .acquire("key", Some(1), None, wait(), Instant::now())
            .await
            .unwrap();
        let error = limiter
            .acquire(
                "key",
                Some(1),
                None,
                Duration::from_millis(30),
                Instant::now(),
            )
            .await
            .expect_err("a waited-out request must fail");
        assert_eq!(error.status(), 429);
        // The abandoned ticket is not left in the queue.
        assert_eq!(limiter.queued("key"), 0);
    }

    #[tokio::test]
    async fn an_unlimited_key_never_queues() {
        let limiter = Limiter::new();
        let first = limiter
            .acquire("key", None, None, wait(), Instant::now())
            .await
            .unwrap();
        let second = limiter
            .acquire("key", None, None, wait(), Instant::now())
            .await
            .unwrap();
        // The no-op permits do not count against a lane that has no limit.
        assert_eq!(limiter.in_flight("key"), 0);
        drop((first, second));
    }

    #[tokio::test]
    async fn the_minute_window_allows_exactly_its_limit() {
        let limiter = Limiter::new();
        for _ in 0..3 {
            limiter
                .reserve_minute("key", Some(3), Duration::from_millis(1))
                .await
                .unwrap();
        }
        let refused = limiter
            .reserve_minute("key", Some(3), Duration::from_millis(1))
            .await;
        let error = refused.expect_err("the fourth request exceeds the minute");
        assert_eq!(error.status(), 429);
    }

    #[tokio::test]
    async fn a_request_that_queued_still_counts_in_the_minute_it_leaves() {
        let limiter = Limiter::new();
        limiter
            .reserve_minute("key", Some(1), Duration::from_millis(1))
            .await
            .unwrap();
        // The window is full, and the wait is longer than the deadline.
        let error = limiter
            .reserve_minute("key", Some(1), Duration::from_millis(1))
            .await
            .expect_err("the second request must wait and then give up");
        assert_eq!(error.status(), 429);
        assert!(error.message().contains("minute"), "{}", error.message());
    }

    #[tokio::test]
    async fn limits_are_per_account() {
        let limiter = Limiter::new();
        let _first = limiter
            .acquire("key-a", Some(1), None, wait(), Instant::now())
            .await
            .unwrap();
        // A different Key has its own lane.
        let second = limiter
            .acquire("key-b", Some(1), None, wait(), Instant::now())
            .await
            .unwrap();
        assert_eq!(limiter.in_flight("key-a"), 1);
        assert_eq!(limiter.in_flight("key-b"), 1);
        drop(second);
    }
}
