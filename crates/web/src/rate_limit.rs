//! Rate limits for login and registration, against password guessing and
//! signup floods.
//!
//! Counters live in this process's memory, so with several web nodes each
//! node counts separately; a shared (Valkey) store comes with M8.

use std::net::IpAddr;
use std::num::NonZeroU32;
use std::time::Duration;

use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};

use crate::error::AppError;

pub struct RateLimits {
    login_by_ip: DefaultKeyedRateLimiter<IpAddr>,
    login_by_name: DefaultKeyedRateLimiter<String>,
    register_by_ip: DefaultKeyedRateLimiter<IpAddr>,
}

/// `burst` attempts at once, then one more every `period`.
fn quota(burst: u32, period: Duration) -> Quota {
    Quota::with_period(period)
        .expect("period is non-zero")
        .allow_burst(NonZeroU32::new(burst).expect("burst is non-zero"))
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            // Generous per IP, since many people can share one (NAT, campus).
            login_by_ip: RateLimiter::keyed(quota(20, Duration::from_secs(6))),
            // Tight per account: guessing one account's password from many IPs.
            login_by_name: RateLimiter::keyed(quota(5, Duration::from_secs(30))),
            register_by_ip: RateLimiter::keyed(quota(5, Duration::from_secs(12 * 60))),
        }
    }
}

impl RateLimits {
    /// Counts a login attempt. `ip` is `None` only when the connection
    /// address is unknown (in-process tests).
    pub fn check_login(&self, ip: Option<IpAddr>, name: &str) -> Result<(), AppError> {
        if let Some(ip) = ip {
            check(&self.login_by_ip, &ip)?;
        }
        check(&self.login_by_name, &name.to_lowercase())
    }

    pub fn check_register(&self, ip: Option<IpAddr>) -> Result<(), AppError> {
        match ip {
            Some(ip) => check(&self.register_by_ip, &ip),
            None => Ok(()),
        }
    }

    /// Forgets keys that are back at full allowance, bounding memory use.
    pub fn retain_recent(&self) {
        self.login_by_ip.retain_recent();
        self.login_by_name.retain_recent();
        self.register_by_ip.retain_recent();
    }
}

fn check<K: std::hash::Hash + Eq + Clone>(
    limiter: &DefaultKeyedRateLimiter<K>,
    key: &K,
) -> Result<(), AppError> {
    limiter.check_key(key).map_err(|not_until| {
        let wait = not_until.wait_time_from(DefaultClock::default().now());
        AppError::TooManyRequests {
            retry_after_secs: wait.as_secs().max(1),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> Option<IpAddr> {
        Some(IpAddr::from([198, 51, 100, last]))
    }

    #[test]
    fn limits_attempts_per_account_across_ips() {
        let limits = RateLimits::default();
        for i in 0..5 {
            limits.check_login(ip(i), "alice").unwrap();
        }
        let err = limits.check_login(ip(99), "ALICE").unwrap_err();
        assert!(
            matches!(err, AppError::TooManyRequests { retry_after_secs } if retry_after_secs >= 1)
        );
        // Other accounts are unaffected.
        limits.check_login(ip(99), "bob").unwrap();
    }

    #[test]
    fn limits_attempts_per_ip_across_accounts() {
        let limits = RateLimits::default();
        for i in 0..20 {
            limits.check_login(ip(1), &format!("user{i}")).unwrap();
        }
        assert!(limits.check_login(ip(1), "someone-else").is_err());
        limits.check_login(ip(2), "someone-else").unwrap();
    }

    #[test]
    fn limits_registrations_per_ip() {
        let limits = RateLimits::default();
        for _ in 0..5 {
            limits.check_register(ip(1)).unwrap();
        }
        assert!(limits.check_register(ip(1)).is_err());
        limits.check_register(None).unwrap();
    }
}
