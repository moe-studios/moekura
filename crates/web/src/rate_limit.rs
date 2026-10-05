//! Rate limits for login, registration and forms that send email, against
//! password guessing, signup floods and mail bombing; and for the APIs, as
//! a whole, per client (`server.api_requests_per_minute`).
//!
//! Counters live in this process's memory, or in Valkey when
//! `cache.backend = "valkey"`, so that several web servers share them.
//! Both use the same algorithm (GCRA: a burst, then one more every
//! period). If Valkey can't be reached, each server falls back to its own
//! counters rather than refusing everyone.
//!
//! Addresses are counted by [`ip_bucket`] (IPv6 by its /64), and names
//! and email addresses by a digest of a fixed size, so a client can't
//! get a fresh allowance by moving within its network, or make a counter
//! as big as the text it sends.

use std::net::IpAddr;
use std::num::NonZeroU32;
use std::time::Duration;

use governor::clock::{Clock, DefaultClock};
use governor::middleware::StateInformationMiddleware;
use governor::state::keyed::DefaultKeyedStateStore;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use ipnet::IpNet;
use moekura_core::accounts::EMAIL_MAX_LEN;
use sha2::{Digest, Sha256};

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
// Tight per account and network: guessing one account's password. Kept
// apart per network, so someone guessing can't lock the owner out.
const LOGIN_BY_NAME_AND_NET: Limit = Limit {
    name: "login_name_net",
    burst: 5,
    period: Duration::from_secs(30),
};
// Per account, from any network: guessing it from many at once. Twice
// the burst of the limit per network, and the same rate, so one network
// alone can't use it up. Past it, the login page still lets the owner in
// with a solved captcha, or from a network the account has used (see
// `account::login`).
const LOGIN_BY_NAME: Limit = Limit {
    name: "login_name",
    burst: 10,
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

// Posting comments, against spam and floods.
const COMMENT_BY_USER: Limit = Limit {
    name: "comment_user",
    burst: 5,
    period: Duration::from_secs(20),
};

// Flagging posts and reporting comments (one allowance for both): each
// lands in a staff queue, and a flag marks the post flagged.
const REPORT_BY_USER: Limit = Limit {
    name: "report_user",
    burst: 10,
    period: Duration::from_secs(60),
};

// Appealing deleted posts: each lands in the staff's queue.
const APPEAL_BY_USER: Limit = Limit {
    name: "appeal_user",
    burst: 3,
    period: Duration::from_secs(4 * 60 * 60),
};

/// A keyed limiter that also says how much allowance is left.
type InfoLimiter =
    RateLimiter<String, DefaultKeyedStateStore<String>, DefaultClock, StateInformationMiddleware>;

/// A client's API allowance after a request, for the `X-RateLimit-*`
/// headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowance {
    /// Requests that may come at once.
    pub limit: u32,
    pub remaining: u32,
    /// How long until the whole burst is available again.
    pub reset: Duration,
    /// Set when the request was refused: how long to wait.
    pub retry_after: Option<Duration>,
}

impl Allowance {
    /// From a GCRA state that is `ahead` of now, for [`Valkey`] counts.
    fn from_ahead(limit: Limit, ahead: Duration, retry_after: Option<Duration>) -> Self {
        let window = limit.period * limit.burst;
        let free = window.saturating_sub(ahead).as_millis() / limit.period.as_millis().max(1);
        Self {
            limit: limit.burst,
            remaining: if retry_after.is_some() {
                0
            } else {
                u32::try_from(free).unwrap_or(u32::MAX).min(limit.burst)
            },
            reset: ahead,
            retry_after,
        }
    }
}

// Private messages, against spam: a few at once, then one a minute.
const DMAIL_BY_USER: Limit = Limit {
    name: "dmail_user",
    burst: 10,
    period: Duration::from_secs(60),
};

// Searching by image: each compares the file with every post's.
const IMAGE_SEARCH: Limit = Limit {
    name: "image_search",
    burst: 10,
    period: Duration::from_secs(12),
};

// Looking up a link on its site (the artist finder, related tags, the
// upload source panel): each fetches a page from another server.
const SOURCE_LOOKUP: Limit = Limit {
    name: "source_lookup",
    burst: 10,
    period: Duration::from_secs(6),
};

// Uploads, against one account flooding the media workers.
const UPLOAD_BY_USER: Limit = Limit {
    name: "upload_user",
    burst: 20,
    period: Duration::from_secs(30),
};

// Tag and bulk update requests: each opens a forum topic and lands in the
// staff's queue.
const REQUEST_BY_USER: Limit = Limit {
    name: "request_user",
    burst: 5,
    period: Duration::from_secs(60),
};

fn quota(limit: Limit) -> Quota {
    Quota::with_period(limit.period)
        .expect("period is non-zero")
        .allow_burst(NonZeroU32::new(limit.burst).expect("burst is non-zero"))
}

pub struct RateLimits {
    login_by_ip: DefaultKeyedRateLimiter<IpAddr>,
    login_by_name_and_net: DefaultKeyedRateLimiter<String>,
    login_by_name: DefaultKeyedRateLimiter<String>,
    register_by_ip: DefaultKeyedRateLimiter<IpAddr>,
    mail_by_ip: DefaultKeyedRateLimiter<IpAddr>,
    mail_by_address: DefaultKeyedRateLimiter<String>,
    code_by_user: DefaultKeyedRateLimiter<i64>,
    confirm_by_user: DefaultKeyedRateLimiter<i64>,
    comment_by_user: DefaultKeyedRateLimiter<i64>,
    report_by_user: DefaultKeyedRateLimiter<i64>,
    appeal_by_user: DefaultKeyedRateLimiter<i64>,
    image_search: DefaultKeyedRateLimiter<String>,
    dmail_by_user: DefaultKeyedRateLimiter<i64>,
    source_lookup: DefaultKeyedRateLimiter<String>,
    upload_by_user: DefaultKeyedRateLimiter<i64>,
    request_by_user: DefaultKeyedRateLimiter<i64>,
    /// Off when `server.api_requests_per_minute` is 0.
    api: Option<(Limit, InfoLimiter)>,
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
            login_by_name_and_net: RateLimiter::keyed(quota(LOGIN_BY_NAME_AND_NET)),
            login_by_name: RateLimiter::keyed(quota(LOGIN_BY_NAME)),
            register_by_ip: RateLimiter::keyed(quota(REGISTER_BY_IP)),
            mail_by_ip: RateLimiter::keyed(quota(MAIL_BY_IP)),
            mail_by_address: RateLimiter::keyed(quota(MAIL_BY_ADDRESS)),
            code_by_user: RateLimiter::keyed(quota(CODE_BY_USER)),
            confirm_by_user: RateLimiter::keyed(quota(CONFIRM_BY_USER)),
            comment_by_user: RateLimiter::keyed(quota(COMMENT_BY_USER)),
            report_by_user: RateLimiter::keyed(quota(REPORT_BY_USER)),
            appeal_by_user: RateLimiter::keyed(quota(APPEAL_BY_USER)),
            image_search: RateLimiter::keyed(quota(IMAGE_SEARCH)),
            dmail_by_user: RateLimiter::keyed(quota(DMAIL_BY_USER)),
            source_lookup: RateLimiter::keyed(quota(SOURCE_LOOKUP)),
            upload_by_user: RateLimiter::keyed(quota(UPLOAD_BY_USER)),
            request_by_user: RateLimiter::keyed(quota(REQUEST_BY_USER)),
            api: None,
            valkey,
        }
    }

    /// Limits API clients to `per_minute` requests a minute, `burst` at
    /// once; 0 a minute is no limit.
    pub fn with_api_limit(mut self, per_minute: u32, burst: u32) -> Self {
        self.api = (per_minute > 0 && burst > 0).then(|| {
            let limit = Limit {
                name: "api",
                burst,
                period: Duration::from_secs(60) / per_minute,
            };
            let limiter =
                RateLimiter::keyed(quota(limit)).with_middleware::<StateInformationMiddleware>();
            (limit, limiter)
        });
        self
    }

    /// Counts an API request by `client` (`user:<id>` or `ip:<address>`):
    /// the allowance left, or `None` when the API isn't limited.
    pub async fn check_api(&self, client: &str) -> Option<Allowance> {
        let (limit, local) = self.api.as_ref()?;
        let limit = *limit;
        if let Some(valkey) = &self.valkey {
            let key = format!("rate:{}:{client}", limit.name);
            match valkey.gcra_state(&key, limit.burst, limit.period).await {
                Ok((wait, ahead)) => return Some(Allowance::from_ahead(limit, ahead, wait)),
                Err(error) => {
                    tracing::warn!(%error, "Valkey unavailable; rate limiting in this process only");
                }
            }
        }
        let key = client.to_owned();
        Some(match local.check_key(&key) {
            Ok(snapshot) => {
                let remaining = snapshot.remaining_burst_capacity();
                Allowance {
                    limit: limit.burst,
                    remaining,
                    reset: limit.period * (limit.burst - remaining.min(limit.burst)),
                    retry_after: None,
                }
            }
            Err(not_until) => {
                let wait = not_until.wait_time_from(DefaultClock::default().now());
                Allowance {
                    limit: limit.burst,
                    remaining: 0,
                    reset: wait + limit.period * (limit.burst - 1),
                    retry_after: Some(wait),
                }
            }
        })
    }

    /// Counts a login attempt for account `name`: from the client's
    /// network, and for the account from that network. Every attempt
    /// counts, before the password is checked, so that attempts sent all
    /// at once can't slip past. `ip` is `None` only when the connection
    /// address is unknown (in-process tests).
    pub async fn check_login(&self, ip: Option<IpAddr>, name: &str) -> Result<(), AppError> {
        let net = ip.map(ip_bucket);
        if let Some(net) = net {
            self.check(LOGIN_BY_IP, &self.login_by_ip, &net, &net.to_string())
                .await?;
        }
        let name = fold(name);
        // A network's text has no spaces, so this can't be another pair's.
        let from_net = digest(&format!(
            "{} {name}",
            net.map(|n| n.to_string()).unwrap_or_default()
        ));
        self.check(
            LOGIN_BY_NAME_AND_NET,
            &self.login_by_name_and_net,
            &from_net,
            &from_net,
        )
        .await
    }

    /// Counts a login attempt for account `name` from any network, against
    /// guessing it from many at once. Counted before the password is
    /// checked, like [`Self::check_login`].
    pub async fn check_login_ceiling(&self, name: &str) -> Result<(), AppError> {
        let name = digest(&fold(name));
        self.check(LOGIN_BY_NAME, &self.login_by_name, &name, &name)
            .await
    }

    pub async fn check_register(&self, ip: Option<IpAddr>) -> Result<(), AppError> {
        match ip.map(ip_bucket) {
            Some(net) => {
                self.check(REGISTER_BY_IP, &self.register_by_ip, &net, &net.to_string())
                    .await
            }
            None => Ok(()),
        }
    }

    /// Counts a request that would email `address` (a name or an email
    /// address, whatever the form asked for). Refuses one longer than any
    /// account's without counting it.
    pub async fn check_mail(&self, ip: Option<IpAddr>, address: &str) -> Result<(), AppError> {
        let address = address.trim();
        if address.len() > EMAIL_MAX_LEN {
            return Err(AppError::Unprocessable(
                "No account has a name or email address that long.".into(),
            ));
        }
        if let Some(net) = ip.map(ip_bucket) {
            self.check(MAIL_BY_IP, &self.mail_by_ip, &net, &net.to_string())
                .await?;
        }
        let address = digest(&fold(address));
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

    /// Counts a comment posted by user `user_id`.
    pub async fn check_comment(&self, user_id: i64) -> Result<(), AppError> {
        self.check(
            COMMENT_BY_USER,
            &self.comment_by_user,
            &user_id,
            &user_id.to_string(),
        )
        .await
    }

    /// Counts a flag or comment report by user `user_id`.
    pub async fn check_report(&self, user_id: i64) -> Result<(), AppError> {
        self.check(
            REPORT_BY_USER,
            &self.report_by_user,
            &user_id,
            &user_id.to_string(),
        )
        .await
    }

    /// Counts an appeal of a deleted post by user `user_id`.
    pub async fn check_appeal(&self, user_id: i64) -> Result<(), AppError> {
        self.check(
            APPEAL_BY_USER,
            &self.appeal_by_user,
            &user_id,
            &user_id.to_string(),
        )
        .await
    }

    /// Counts a private message sent by user `user_id`.
    pub async fn check_dmail(&self, user_id: i64) -> Result<(), AppError> {
        self.check(
            DMAIL_BY_USER,
            &self.dmail_by_user,
            &user_id,
            &user_id.to_string(),
        )
        .await
    }

    /// Counts a search by image by `client` (`user:<id>` or `ip:<address>`).
    pub async fn check_image_search(&self, client: &str) -> Result<(), AppError> {
        let key = client.to_owned();
        self.check(IMAGE_SEARCH, &self.image_search, &key, &key)
            .await
    }

    /// Counts a lookup of a link on its site by `client` (`user:<id>` or
    /// `ip:<address>`, see [`client_key`]).
    pub async fn check_source_lookup(&self, client: &str) -> Result<(), AppError> {
        let key = client.to_owned();
        self.check(SOURCE_LOOKUP, &self.source_lookup, &key, &key)
            .await
    }

    /// Counts an upload (a file or a link) by user `user_id`.
    pub async fn check_upload(&self, user_id: i64) -> Result<(), AppError> {
        self.check(
            UPLOAD_BY_USER,
            &self.upload_by_user,
            &user_id,
            &user_id.to_string(),
        )
        .await
    }

    /// Counts a tag or bulk update request by user `user_id`.
    pub async fn check_request(&self, user_id: i64) -> Result<(), AppError> {
        self.check(
            REQUEST_BY_USER,
            &self.request_by_user,
            &user_id,
            &user_id.to_string(),
        )
        .await
    }

    /// Forgets keys that are back at full allowance, bounding memory use.
    /// (Valkey expires its keys itself.)
    pub fn retain_recent(&self) {
        self.login_by_ip.retain_recent();
        self.login_by_name_and_net.retain_recent();
        self.login_by_name.retain_recent();
        self.register_by_ip.retain_recent();
        self.mail_by_ip.retain_recent();
        self.mail_by_address.retain_recent();
        self.code_by_user.retain_recent();
        self.confirm_by_user.retain_recent();
        self.comment_by_user.retain_recent();
        self.report_by_user.retain_recent();
        self.appeal_by_user.retain_recent();
        self.image_search.retain_recent();
        self.dmail_by_user.retain_recent();
        self.source_lookup.retain_recent();
        self.upload_by_user.retain_recent();
        self.request_by_user.retain_recent();
        if let Some((_, api)) = &self.api {
            api.retain_recent();
        }
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

/// The address a per-IP limit counts: IPv4 as it is (also when it comes
/// IPv4-mapped, as `::ffff:a.b.c.d`), IPv6 by its /64, which one person
/// usually has.
pub(crate) fn ip_bucket(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(ipnet::Ipv6Net::new(v6, 64).map_or(v6, |net| net.network())),
        },
        v4 => v4,
    }
}

/// [`ip_bucket`] as a range: an IPv4 address alone, or an IPv6 /64.
pub(crate) fn ip_bucket_net(ip: IpAddr) -> IpNet {
    let bucket = ip_bucket(ip);
    let prefix = if bucket.is_ipv4() { 32 } else { 64 };
    IpNet::new(bucket, prefix).unwrap_or(IpNet::from(bucket))
}

/// A key of a fixed size for text a client chose (a name or an email
/// address), so a long one costs no more memory than a short one.
fn digest(text: &str) -> String {
    hex::encode(&Sha256::digest(text.as_bytes())[..16])
}

/// A name or email address as one counter, however it's written: lower
/// case, with only its ASCII left. The database finds accounts ignoring
/// case by its own rules (`citext`), which can take `İ` for `i`; dropping
/// what isn't ASCII after lowering keeps every spelling it would take for
/// the same account on one counter, rather than each getting its own.
fn fold(text: &str) -> String {
    text.to_lowercase().chars().filter(char::is_ascii).collect()
}

/// Who a limit counts: an account, or else an address (by
/// [`ip_bucket`]).
pub(crate) fn client_key(user: Option<i64>, ip: Option<IpAddr>) -> String {
    match (user, ip) {
        (Some(id), _) => format!("user:{id}"),
        (None, Some(ip)) => format!("ip:{}", ip_bucket(ip)),
        // In-process tests only.
        (None, None) => "ip:unknown".to_owned(),
    }
}

/// Middleware: counts requests to the APIs against their client's
/// allowance and says what's left in `X-RateLimit-Limit`, `-Remaining`
/// and `-Reset` (Unix time when the full burst is back), refusing with 429
/// and `Retry-After` once it's used up. Clients are accounts, or for
/// visitors, addresses (IPv6 by /64, which one person usually has).
pub async fn limit_api(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let path = request.uri().path();
    let is_api = (crate::api::is_api_path(path) && path != crate::api::DOCS)
        || crate::danbooru::is_danbooru_path(path);
    if !is_api {
        return next.run(request).await;
    }
    let user = request
        .extensions()
        .get::<crate::auth::CurrentUser>()
        .and_then(|c| c.user.as_ref())
        .map(|u| u.id);
    let (parts, body) = request.into_parts();
    let ip = crate::client_ip::client_ip(&parts, &state.config.server.trusted_proxies);
    let client = client_key(user, ip);
    let Some(allowance) = state.rate_limits.check_api(&client).await else {
        return next
            .run(axum::extract::Request::from_parts(parts, body))
            .await;
    };
    let mut response = match allowance.retry_after {
        Some(wait) => too_many(wait).into_response(),
        None => {
            next.run(axum::extract::Request::from_parts(parts, body))
                .await
        }
    };
    let reset = std::time::SystemTime::now()
        .checked_add(allowance.reset)
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |at| at.as_secs_f64().ceil() as u64);
    let headers = response.headers_mut();
    for (name, value) in [
        ("x-ratelimit-limit", u64::from(allowance.limit)),
        ("x-ratelimit-remaining", u64::from(allowance.remaining)),
        ("x-ratelimit-reset", reset),
    ] {
        headers.insert(name, axum::http::HeaderValue::from(value));
    }
    response
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
    async fn limits_attempts_per_account_and_network() {
        use crate::shared::tests::{unique, unique_ip};

        for limits in backends().await {
            // A name no other run has used, since Valkey keeps counts.
            let name = unique("alice");
            let guesser = Some(unique_ip());
            for _ in 0..5 {
                limits.check_login(guesser, &name).await.unwrap();
            }
            let err = limits
                .check_login(guesser, &name.to_uppercase())
                .await
                .unwrap_err();
            assert!(
                matches!(err, AppError::TooManyRequests { retry_after_secs } if retry_after_secs >= 1)
            );
            // The owner, on another network, isn't locked out, and the
            // guesser's other accounts are unaffected.
            limits.check_login(Some(unique_ip()), &name).await.unwrap();
            limits.check_login(guesser, &unique("bob")).await.unwrap();
        }
    }

    #[tokio::test]
    async fn limits_attempts_per_account_across_networks() {
        use crate::shared::tests::{unique, unique_ip};

        for limits in backends().await {
            let name = unique("alice");
            for _ in 0..2 {
                let network = Some(unique_ip());
                for _ in 0..5 {
                    limits.check_login(network, &name).await.unwrap();
                    limits.check_login_ceiling(&name).await.unwrap();
                }
            }
            // A third network passes its own limit, not the account's.
            limits.check_login(Some(unique_ip()), &name).await.unwrap();
            let err = limits
                .check_login_ceiling(&name.to_uppercase())
                .await
                .unwrap_err();
            assert!(matches!(err, AppError::TooManyRequests { .. }), "{err:?}");
            limits.check_login_ceiling(&unique("bob")).await.unwrap();
        }
    }

    #[tokio::test]
    async fn spellings_of_one_name_share_counters() {
        use crate::shared::tests::{unique, unique_ip};

        for limits in backends().await {
            let name = unique("alice");
            let from = Some(unique_ip());
            // Postgres can take a dotted capital I for an i.
            let spellings = [
                name.clone(),
                name.to_uppercase(),
                name.replace('i', "İ"),
                name.replace('a', "A"),
                name.replacen('e', "E", 1),
            ];
            for spelling in &spellings {
                limits.check_login(from, spelling).await.unwrap();
            }
            let other = name.replace('l', "L").replace('i', "İ");
            assert!(limits.check_login(from, &other).await.is_err());

            let address = format!("{name}@example.com");
            for spelling in [
                address.clone(),
                address.replace('i', "İ"),
                address.to_uppercase(),
            ] {
                limits
                    .check_mail(Some(unique_ip()), &spelling)
                    .await
                    .unwrap();
            }
            let again = address.replace('e', "E");
            assert!(limits.check_mail(Some(unique_ip()), &again).await.is_err());
        }
    }

    #[test]
    fn folding_keeps_one_spelling() {
        assert_eq!(fold("ALİCE"), "alice");
        assert_eq!(fold("\u{212A}ate"), "kate");
        assert_eq!(fold("Josè@Example.com"), "jos@example.com");
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

    /// `count` addresses in one fresh IPv6 /64.
    fn one_64(count: u128) -> Vec<Option<IpAddr>> {
        let IpAddr::V6(base) = crate::shared::tests::unique_ip() else {
            unreachable!("unique_ip is IPv6")
        };
        let net = u128::from(base) & !u128::from(u64::MAX);
        (1..=count)
            .map(|host| Some(IpAddr::V6((net | (host << 20)).into())))
            .collect()
    }

    #[tokio::test]
    async fn per_ip_limits_count_ipv6_by_64() {
        for limits in backends().await {
            let neighbours = one_64(6);
            for ip in &neighbours[..5] {
                limits.check_register(*ip).await.unwrap();
            }
            assert!(limits.check_register(neighbours[5]).await.is_err());
            limits
                .check_register(Some(crate::shared::tests::unique_ip()))
                .await
                .unwrap();

            let neighbours = one_64(21);
            for (i, ip) in neighbours[..20].iter().enumerate() {
                let name = crate::shared::tests::unique(&format!("u{i}"));
                limits.check_login(*ip, &name).await.unwrap();
            }
            assert!(
                limits
                    .check_login(neighbours[20], &crate::shared::tests::unique("v"))
                    .await
                    .is_err()
            );

            let neighbours = one_64(6);
            for (i, ip) in neighbours[..5].iter().enumerate() {
                let address = crate::shared::tests::unique(&format!("m{i}"));
                limits.check_mail(*ip, &address).await.unwrap();
            }
            let address = crate::shared::tests::unique("n");
            assert!(limits.check_mail(neighbours[5], &address).await.is_err());
        }
    }

    #[tokio::test]
    async fn mail_limits_refuse_impossible_addresses_uncounted() {
        for limits in backends().await {
            let address = crate::shared::tests::unique_ip();
            let long = format!("{}@example.com", "a".repeat(EMAIL_MAX_LEN));
            for _ in 0..10 {
                let err = limits.check_mail(Some(address), &long).await.unwrap_err();
                assert!(matches!(err, AppError::Unprocessable(_)), "{err:?}");
            }
            // None of those used the address's allowance.
            for i in 0..5 {
                let to = crate::shared::tests::unique(&format!("a{i}"));
                limits.check_mail(Some(address), &to).await.unwrap();
            }
        }
    }

    #[test]
    fn keys_for_names_have_a_fixed_size() {
        let long = "a".repeat(2 * 1024 * 1024);
        assert_eq!(digest(&long).len(), 32);
        assert_eq!(digest("alice").len(), 32);
        assert_ne!(digest("alice"), digest("bob"));
    }

    #[test]
    fn buckets_ipv6_by_64_and_unmaps_ipv4() {
        let v6 = |s: &str| IpAddr::V6(s.parse().unwrap());
        assert_eq!(ip_bucket(v6("2001:db8:1:2:aaaa::1")), v6("2001:db8:1:2::"));
        assert_eq!(
            ip_bucket(v6("2001:db8:1:2:bbbb::9")),
            ip_bucket(v6("2001:db8:1:2:aaaa::1"))
        );
        assert_eq!(
            ip_bucket(v6("::ffff:198.51.100.7")),
            IpAddr::from([198, 51, 100, 7])
        );
        assert_eq!(ip_bucket(ip(7).unwrap()), ip(7).unwrap());
        assert_eq!(
            client_key(None, Some(v6("::ffff:198.51.100.7"))),
            "ip:198.51.100.7"
        );
        let net = |s: &str| s.parse::<IpNet>().unwrap();
        assert_eq!(
            ip_bucket_net(v6("2001:db8:1:2:aaaa::1")),
            net("2001:db8:1:2::/64")
        );
        assert_eq!(
            ip_bucket_net(v6("::ffff:198.51.100.7")),
            net("198.51.100.7/32")
        );
    }

    #[tokio::test]
    async fn limits_uploads_lookups_and_requests() {
        for limits in backends().await {
            let id = crate::shared::tests::fresh() as i64 & i64::MAX;
            for _ in 0..20 {
                limits.check_upload(id).await.unwrap();
            }
            assert!(limits.check_upload(id).await.is_err());
            for _ in 0..5 {
                limits.check_request(id).await.unwrap();
            }
            assert!(limits.check_request(id).await.is_err());
            let client = format!("ip:{}", crate::shared::tests::unique_ip());
            for _ in 0..10 {
                limits.check_source_lookup(&client).await.unwrap();
            }
            assert!(limits.check_source_lookup(&client).await.is_err());
        }
    }

    #[tokio::test]
    async fn limits_flags_and_reports_per_user() {
        for limits in backends().await {
            // Ids no other run has used, since Valkey keeps counts.
            let id = || crate::shared::tests::fresh() as i64 & i64::MAX;
            let user = id();
            for _ in 0..10 {
                limits.check_report(user).await.unwrap();
            }
            assert!(limits.check_report(user).await.is_err());
            limits.check_report(id()).await.unwrap();
        }
    }

    #[tokio::test]
    async fn counts_api_requests_per_client() {
        assert_eq!(RateLimits::default().check_api("ip:x").await, None);
        for limits in backends().await {
            // A burst of 3, then one every 20 seconds.
            let limits = limits.with_api_limit(3, 3);
            let client = crate::shared::tests::unique("user");
            let first = limits.check_api(&client).await.unwrap();
            assert_eq!((first.limit, first.remaining), (3, 2));
            assert!(first.retry_after.is_none());
            assert!(first.reset > Duration::from_secs(15), "{first:?}");
            limits.check_api(&client).await.unwrap();
            let last = limits.check_api(&client).await.unwrap();
            assert_eq!(last.remaining, 0);
            let refused = limits.check_api(&client).await.unwrap();
            let wait = refused.retry_after.expect("refused");
            assert!(
                wait > Duration::from_secs(15) && wait <= Duration::from_secs(20),
                "{wait:?}"
            );
            assert!(refused.reset >= Duration::from_secs(55), "{refused:?}");
            // Others have their own allowance.
            let other = limits
                .check_api(&crate::shared::tests::unique("user"))
                .await
                .unwrap();
            assert_eq!(other.remaining, 2);
        }
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn api_responses_carry_the_allowance(pool: sqlx::PgPool) {
        use axum::http::StatusCode;

        use crate::test_support::{TestApp, test_config, test_state_with};

        let mut config = test_config();
        config.server.api_requests_per_minute = 2;
        config.server.api_burst = 2;
        let app = TestApp::new(
            test_state_with(&pool, config).await,
            crate::api::routes(1024)
                .merge(crate::danbooru::routes(1024 * 1024))
                .merge(crate::posts::routes()),
        );
        let first = app.get_full("/api/v1/posts").await;
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(first.headers()["x-ratelimit-limit"], "2");
        assert_eq!(first.headers()["x-ratelimit-remaining"], "1");
        let reset: u64 = first.headers()["x-ratelimit-reset"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(reset > now + 20 && reset <= now + 31, "{reset} vs {now}");
        // The Danbooru API shares the allowance.
        let second = app.get_full("/posts.json").await;
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(second.headers()["x-ratelimit-remaining"], "0");
        let refused = app.get("/api/v1/posts", None).await;
        assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            refused.retry_after.is_some_and(|s| s > 20),
            "{:?}",
            refused.retry_after
        );
        assert!(refused.body.contains("\"status\":429"), "{}", refused.body);
        // Pages aren't counted.
        let page = app.get_full("/").await;
        assert_eq!(page.status(), StatusCode::OK);
        assert!(!page.headers().contains_key("x-ratelimit-limit"));
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
