use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Entry {
    count: u32,
    first_attempt: Instant,
    locked_until: Option<Instant>,
}

pub struct BruteForceGuard {
    inner: Mutex<HashMap<String, Entry>>,
    max_attempts: u32,
    window: Duration,
    lockout: Duration,
    last_sweep: Mutex<Instant>,
}

impl BruteForceGuard {
    pub fn new(max_attempts: u32, window_secs: u64, lockout_secs: u64) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max_attempts,
            window: Duration::from_secs(window_secs),
            lockout: Duration::from_secs(lockout_secs),
            last_sweep: Mutex::new(Instant::now()),
        }
    }

    fn key(ip: &str, username: &str) -> String {
        format!("{ip}:{username}")
    }

    /// Returns Ok(()) if allowed, Err(seconds_remaining) if locked out.
    pub fn check(&self, ip: &str, username: &str) -> Result<(), u64> {
        // Sweep at most every 60 seconds
        {
            let last = self.last_sweep.lock().unwrap();
            if last.elapsed() > Duration::from_secs(60) {
                drop(last);
                self.sweep();
                *self.last_sweep.lock().unwrap() = Instant::now();
            }
        }
        let key = Self::key(ip, username);
        let map = self.inner.lock().unwrap();
        let now = Instant::now();

        if let Some(entry) = map.get(&key) {
            if let Some(locked_until) = entry.locked_until {
                if now < locked_until {
                    let remaining = (locked_until - now).as_secs() + 1;
                    return Err(remaining);
                }
            }
        }
        Ok(())
    }

    /// Record a failed attempt. Returns Err(lockout_secs) if this attempt triggers lockout.
    pub fn record_failure(&self, ip: &str, username: &str) -> Result<(), u64> {
        let key = Self::key(ip, username);
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();

        let entry = map.entry(key).or_insert(Entry {
            count: 0,
            first_attempt: now,
            locked_until: None,
        });

        // Reset window if previous window has passed
        if now.duration_since(entry.first_attempt) > self.window {
            entry.count = 0;
            entry.first_attempt = now;
            entry.locked_until = None;
        }

        entry.count += 1;

        if entry.count >= self.max_attempts {
            let until = now + self.lockout;
            entry.locked_until = Some(until);
            let map_len = map.len();
            drop(map);
            if map_len > 10_000 {
                self.sweep();
            }
            return Err(self.lockout.as_secs());
        }

        if map.len() > 10_000 {
            drop(map);
            self.sweep();
        }

        Ok(())
    }

    /// Clear entry on successful login.
    pub fn record_success(&self, ip: &str, username: &str) {
        let key = Self::key(ip, username);
        self.inner.lock().unwrap().remove(&key);
    }

    /// Sweep expired entries (call periodically to avoid unbounded growth).
    pub fn sweep(&self) {
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();
        map.retain(|_, entry| {
            // Keep if locked (not expired) or window not yet passed
            if let Some(until) = entry.locked_until {
                return now < until;
            }
            now.duration_since(entry.first_attempt) <= self.window
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lockout_after_max_attempts() {
        let guard = BruteForceGuard::new(3, 300, 900);
        let ip = "127.0.0.1";
        let user = "alice";

        assert!(guard.check(ip, user).is_ok());
        guard.record_failure(ip, user).unwrap();
        guard.record_failure(ip, user).unwrap();
        let r = guard.record_failure(ip, user);
        assert!(r.is_err(), "3rd attempt should trigger lockout");

        let check = guard.check(ip, user);
        assert!(check.is_err(), "should be locked out now");
        let remaining = check.unwrap_err();
        assert!(remaining > 0);
    }

    #[test]
    fn test_success_clears_counter() {
        let guard = BruteForceGuard::new(3, 300, 900);
        let ip = "10.0.0.1";
        let user = "bob";

        guard.record_failure(ip, user).unwrap();
        guard.record_failure(ip, user).unwrap();
        guard.record_success(ip, user);

        // Counter reset → can fail again without immediate lockout
        guard.record_failure(ip, user).unwrap();
        guard.record_failure(ip, user).unwrap();
        assert!(guard.check(ip, user).is_ok());
    }

    #[test]
    fn test_different_users_independent() {
        let guard = BruteForceGuard::new(2, 300, 900);
        let ip = "1.2.3.4";

        guard.record_failure(ip, "alice").unwrap();
        let _ = guard.record_failure(ip, "alice");

        // bob should not be affected
        assert!(guard.check(ip, "bob").is_ok());
    }
}
