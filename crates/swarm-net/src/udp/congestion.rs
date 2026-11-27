//! How much the UDP stream may have in flight, and when to give up waiting.
//!
//! The usual TCP rules without selective acknowledgements: slow start,
//! additive increase and multiplicative decrease (RFC 5681), recovery one
//! hole at a time (RFC 6582), and a retransmission timeout from the measured
//! round trip (RFC 6298). Counted in segments, not bytes.

use std::time::Duration;

/// Most segments in flight, and what a receiver buffers out of order.
pub const MAX_WINDOW: usize = 512;
/// Segments in flight before anything is known about the path.
const FIRST_WINDOW: usize = 10;
/// Duplicate acknowledgements that mean a segment was lost.
pub const DUPLICATES: usize = 3;

const FIRST_RTO: Duration = Duration::from_secs(1);
const MIN_RTO: Duration = Duration::from_millis(200);
const MAX_RTO: Duration = Duration::from_secs(4);

pub struct Congestion {
    window: usize,
    /// Below this the window doubles each round trip, above it grows by one.
    threshold: usize,
    /// Acknowledged segments counted towards the next step of one.
    credit: usize,
    /// Smoothed round trip and its variation, once measured.
    measured: Option<(Duration, Duration)>,
    rto: Duration,
}

impl Congestion {
    pub fn new() -> Self {
        Self {
            window: FIRST_WINDOW,
            threshold: MAX_WINDOW,
            credit: 0,
            measured: None,
            rto: FIRST_RTO,
        }
    }

    pub fn window(&self) -> usize {
        self.window
    }

    /// How long to wait for an acknowledgement before sending again.
    pub fn rto(&self) -> Duration {
        self.rto
    }

    /// `count` segments arrived, outside loss recovery.
    pub fn grow(&mut self, count: usize) {
        if self.window < self.threshold {
            self.window += count.min(self.window);
        } else {
            self.credit += count;
            while self.credit >= self.window {
                self.credit -= self.window;
                self.window += 1;
            }
        }
        self.window = self.window.min(MAX_WINDOW);
    }

    /// A segment is missing but later ones arrive: halve.
    pub fn lost(&mut self, in_flight: usize) {
        self.threshold = (in_flight / 2).max(2);
        self.window = self.threshold;
        self.credit = 0;
    }

    /// Nothing came back at all: start over, and wait longer.
    pub fn timed_out(&mut self, in_flight: usize) {
        self.lost(in_flight);
        self.window = 1;
        self.rto = (self.rto * 2).min(MAX_RTO);
    }

    /// A round trip measured on a segment that was sent only once.
    pub fn sample(&mut self, rtt: Duration) {
        let (smoothed, variation) = match self.measured {
            None => (rtt, rtt / 2),
            Some((smoothed, variation)) => (
                (smoothed * 7 + rtt) / 8,
                (variation * 3 + smoothed.abs_diff(rtt)) / 4,
            ),
        };
        self.measured = Some((smoothed, variation));
        self.reset_rto();
    }

    /// Drop any backoff: the path is answering again.
    pub fn reset_rto(&mut self) {
        self.rto = match self.measured {
            Some((smoothed, variation)) => {
                (smoothed + (variation * 4).max(Duration::from_millis(10))).clamp(MIN_RTO, MAX_RTO)
            }
            None => FIRST_RTO,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_doubles_then_creeps_and_shrinks_on_loss() {
        let mut path = Congestion::new();
        path.grow(10);
        assert_eq!(path.window(), 20);
        path.lost(20);
        assert_eq!(path.window(), 10);
        // Past the threshold: one more per window acknowledged.
        path.grow(9);
        assert_eq!(path.window(), 10);
        path.grow(1);
        assert_eq!(path.window(), 11);
        path.timed_out(11);
        assert_eq!(path.window(), 1);
        for _ in 0..10_000 {
            path.grow(8);
        }
        assert!(path.window() <= MAX_WINDOW);
    }

    #[test]
    fn the_timeout_follows_the_round_trip() {
        let mut path = Congestion::new();
        assert_eq!(path.rto(), FIRST_RTO);
        path.sample(Duration::from_millis(300));
        assert_eq!(path.rto(), Duration::from_millis(900));
        for _ in 0..50 {
            path.sample(Duration::from_millis(1));
        }
        assert_eq!(path.rto(), MIN_RTO);
        for _ in 0..8 {
            path.timed_out(4);
        }
        assert_eq!(path.rto(), MAX_RTO);
    }
}
