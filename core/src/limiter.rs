use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Token-bucket rate limiter (bytes/second). A rate of 0 means unlimited.
/// Callers may go into "debt" and then sleep it off, so arbitrarily large chunks work.
pub struct RateLimiter {
    rate: AtomicU64,
    state: Mutex<(f64, Instant)>,
}

impl RateLimiter {
    pub fn new(rate: u64) -> Self {
        RateLimiter { rate: AtomicU64::new(rate), state: Mutex::new((0.0, Instant::now())) }
    }
    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }
    pub fn set_rate(&self, rate: u64) {
        if self.rate.swap(rate, Ordering::Relaxed) != rate {
            *self.state.lock().unwrap() = (0.0, Instant::now());
        }
    }
    /// Reserve `n` bytes and return how long the caller must wait.
    pub fn reserve(&self, n: u64) -> Duration {
        let rate = self.rate() as f64;
        if rate <= 0.0 {
            return Duration::ZERO;
        }
        let mut st = self.state.lock().unwrap();
        let now = Instant::now();
        let burst = (rate / 8.0).max(16384.0);
        st.0 = (st.0 + now.duration_since(st.1).as_secs_f64() * rate).min(burst);
        st.1 = now;
        st.0 -= n as f64;
        if st.0 < 0.0 { Duration::from_secs_f64(-st.0 / rate) } else { Duration::ZERO }
    }
    pub async fn acquire(&self, n: u64) {
        let d = self.reserve(n);
        if !d.is_zero() {
            tokio::time::sleep(d).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn limits_rate() {
        let l = RateLimiter::new(200_000);
        let t = Instant::now();
        for _ in 0..20 {
            l.acquire(20_000).await; // 400 KB total at 200 KB/s ~= 2s minus burst
        }
        let s = t.elapsed().as_secs_f64();
        assert!(s > 1.6 && s < 4.0, "took {s}");
    }
    #[test]
    fn unlimited() {
        assert!(RateLimiter::new(0).reserve(1 << 30).is_zero());
    }
}
