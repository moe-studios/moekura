//! Rate limits for login, registration and forms that send email, against
//! password guessing, signup floods and mail bombing.
//!
//! Counters live in this process's memory, or in Valkey when
//! `cache.backend = "valkey"`, so that several web servers share them.
//! Both use the same algorithm (GCRA: a burst, then one more every
//! period). If Valkey can't be reached, each server falls back to its own
//! counters rather than refusing everyone.

use std::net::IpAddr;
use std::num::NonZeroU32;
use std::time::Duration;

use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};

use crate::error::AppError;
use crate::shared::Valkey;

/// A limit: `burst` attempts at once, then one more every `period`.
#[derive(Debug, Clone, Copy)]
struct Limit {
    name: &'static str,
    burst: u32,
    period: Duration,
}

// Generous per IP, since many people can share one (NAT, campus).
const LOGIN_BY_IP: Limit = Limit {
    name: "login_ip",
    burst: 20,
    period: Duration::from_secs(6),
};
// Tight per account: guessing one account's password from many IPs.
const LOGIN_BY_NAME: Limit = Limit {
    name: "login_name",
    burst: 5,
    period: Duration::from_secs(30),
};
const REGISTER_BY_IP: Limit = Limit {
    name: "register_ip",
    burst: 5,
    period: Duration::from_secs(12 * 60),
};

// Forms that send a message (password resets, confirmation links).
const MAIL_BY_IP: Limit = Limit {
    name: "mail_ip",
    burst: 5,
    period: Duration::from_secs(2 * 60),
};
// However many IPs ask, one address gets a few messages an hour.
const MAIL_BY_ADDRESS: Limit = Limit {
    name: "mail_address",
    burst: 3,
    period: Duration::from_secs(20 * 60),
};

// Two-factor codes at login. Each login allows only a few before it
// starts over with the password, which counts as a login attempt.
const CODE_BY_USER: Limit = Limit {
    name: "code_user",
    burst: 5,
    period: Duration::from_secs(30),
};
// Retyping the password (or a code) to change account settings, against
// someone guessing it from a session they took over.
const CONFIRM_BY_USER: Limit = Limit {
    name: "confirm_user",
    burst: 10,
    period: Duration::from_secs(30),
};

fn quota(limit: Limit) -> Quota {
    Quota::with_period(limit.period)
        .expect("period is non-zero")
        .allow_burst(NonZeroU32::new(limit.burst).expect("burst is non-zero"))
}

pub struct RateLimits {
    login_by_ip: DefaultKeyedRateLimiter<IpAddr>,
    login_by_name: DefaultKeyedRateLimiter<String>,
    register_by_ip: DefaultKeyedRateLimiter<IpAddr>,
    mail_by_ip: DefaultKeyedRateLimiter<IpAddr>,
    mail_by_address: DefaultKeyedRateLimiter<String>,
    code_by_user: DefaultKeyedRateLimiter<i64>,
    confirm_by_user: DefaultKeyedRateLimiter<i64>,
    valkey: Option<Valkey>,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self::new(None)
    }
}

impl RateLimits {
    /// Counts in `valkey` when given, in memory otherwise.
    pub fn new(valkey: Option<Valkey>) -> Self {
        Self {
            login_by_ip: RateLimiter::keyed(quota(LOGIN_BY_IP)),
            login_by_name: RateLimiter::keyed(quota(LOGIN_BY_NAME)),
            register_by_ip: RateLimiter::keyed(quota(REGISTER_BY_IP)),
            mail_by_ip: RateLimiter::keyed(quota(MAIL_BY_IP)),
            mail_by_address: RateLimiter::keyed(quota(MAIL_BY_ADDRESS)),
            code_by_user: RateLimiter::keyed(quota(CODE_BY_USER)),
            confirm_by_user: RateLimiter::keyed(quota(CONFIRM_BY_USER)),
            valkey,
        }
    }

    /// Counts a login attempt. `ip` is `None` only when the connection
    /// address is unknown (in-process tests).
    pub async fn check_login(&self, ip: Option<IpAddr>, name: &str) -> Result<(), AppError> {
        if let Some(ip) = ip {
            self.check(LOGIN_BY_IP, &self.login_by_ip, &ip, &ip.to_string())
                .await?;
        }
        let name = name.to_lowercase();
        self.check(LOGIN_BY_NAME, &self.login_by_name, &name, &name)
            .await
    }

    pub async fn check_register(&self, ip: Option<IpAddr>) -> Result<(), AppError> {
        match ip {
            Some(ip) => {
                self.check(REGISTER_BY_IP, &self.register_by_ip, &ip, &ip.to_string())
                    .await
            }
            None => Ok(()),
        }
    }

    /// Counts a request that would email `address` (a name or an email
    /// address, whatever the form asked for).
    pub async fn check_mail(&self, ip: Option<IpAddr>, address: &str) -> Result<(), AppError> {
        if let Some(ip) = ip {
            self.check(MAIL_BY_IP, &self.mail_by_ip, &ip, &ip.to_string())
                .await?;
        }
        let address = address.trim().to_lowercase();
        self.check(MAIL_BY_ADDRESS, &self.mail_by_address, &address, &address)
            .await
    }

    /// Counts a two-factor code typed at login by user `user_id`.
    pub async fn check_code(&self, user_id: i64) -> Result<(), AppError> {
        self.check(
            CODE_BY_USER,
            &self.code_by_user,
            &user_id,
            &user_id.to_string(),
        )
        .await
    }

    /// Counts a password (or code) typed to change user `user_id`'s
    /// account settings.
    pub async fn check_confirm(&self, user_id: i64) -> Result<(), AppError> {
        self.check(
            CONFIRM_BY_USER,
            &self.confirm_by_user,
            &user_id,
            &user_id.to_string(),
        )
        .await
    }

    /// Forgets keys that are back at full allowance, bounding memory use.
    /// (Valkey expires its keys itself.)
    pub fn retain_recent(&self) {
        self.login_by_ip.retain_recent();
        self.login_by_name.retain_recent();
        self.register_by_ip.retain_recent();
        self.mail_by_ip.retain_recent();
        self.mail_by_address.retain_recent();
        self.code_by_user.retain_recent();
        self.confirm_by_user.retain_recent();
    }

    async fn check<K: std::hash::Hash + Eq + Clone>(
        &self,
        limit: Limit,
        local: &DefaultKeyedRateLimiter<K>,
        key: &K,
        shared_key: &str,
    ) -> Result<(), AppError> {
        if let Some(valkey) = &self.valkey {
            let key = format!("rate:{}:{shared_key}", limit.name);
            match valkey.gcra(&key, limit.burst, limit.period).await {
                Ok(None) => return Ok(()),
                Ok(Some(wait)) => return Err(too_many(wait)),
                Err(error) => {
                    tracing::warn!(%error, "Valkey unavailable; rate limiting in this process only");
                }
            }
        }
        local
            .check_key(key)
            .map_err(|not_until| too_many(not_until.wait_time_from(DefaultClock::default().now())))
    }
}

fn too_many(wait: Duration) -> AppError {
    AppError::TooManyRequests {
        retry_after_secs: wait.as_secs().max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> Option<IpAddr> {
        Some(IpAddr::from([198, 51, 100, last]))
    }

    /// The same checks, against memory and, when `TEST_VALKEY_URL` is
    /// set, a real Valkey.
    async fn backends() -> Vec<RateLimits> {
        let mut limits = vec![RateLimits::default()];
        if let Some(valkey) = crate::shared::tests::valkey().await {
            limits.push(RateLimits::new(Some(valkey)));
        }
        limits
    }

    #[tokio::test]
    async fn limits_attempts_per_account_across_ips() {
        for limits in backends().await {
            // A name no other run has used, since Valkey keeps counts.
            let name = crate::shared::tests::unique("alice");
            for i in 0..5 {
                limits.check_login(ip(i), &name).await.unwrap();
            }
            let err = limits
                .check_login(ip(99), &name.to_uppercase())
                .await
                .unwrap_err();
            assert!(
                matches!(err, AppError::TooManyRequests { retry_after_secs } if retry_after_secs >= 1)
            );
            // Other accounts are unaffected.
            limits
                .check_login(ip(99), &crate::shared::tests::unique("bob"))
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn limits_attempts_per_ip_across_accounts() {
        for limits in backends().await {
            let address = crate::shared::tests::unique_ip();
            for i in 0..20 {
                limits
                    .check_login(
                        Some(address),
                        &crate::shared::tests::unique(&format!("u{i}")),
                    )
                    .await
                    .unwrap();
            }
            assert!(
                limits
                    .check_login(Some(address), "someone-else")
                    .await
                    .is_err()
            );
            limits
                .check_login(Some(crate::shared::tests::unique_ip()), "someone-else")
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn limits_registrations_per_ip() {
        for limits in backends().await {
            let address = crate::shared::tests::unique_ip();
            for _ in 0..5 {
                limits.check_register(Some(address)).await.unwrap();
            }
            assert!(limits.check_register(Some(address)).await.is_err());
            limits.check_register(None).await.unwrap();
        }
    }

    #[tokio::test]
    async fn falls_back_to_memory_when_valkey_is_down() {
        let Some(valkey) = crate::shared::tests::unreachable().await else {
            return;
        };
        let limits = RateLimits::new(Some(valkey));
        let address = ip(7);
        for _ in 0..5 {
            limits.check_register(address).await.unwrap();
        }
        assert!(limits.check_register(address).await.is_err());
    }
}
