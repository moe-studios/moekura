//! Outbound HTTP that can't be used to reach the server's own network
//! (SSRF): for fetching uploads from links and delivering webhooks.
//!
//! Every connection goes through a resolver that refuses names resolving
//! to loopback, private, link-local and other non-public addresses.
//! Because the check happens when connecting, a name can't pass
//! validation and then resolve elsewhere. IP-literal hosts skip DNS, so
//! callers check them with [`is_public`] for the first request and every
//! redirect. Only a few names are looked up at once, so names that are
//! slow to answer can't take over tokio's blocking threads.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect;
use tokio::sync::Semaphore;

/// Whether `ip` is an ordinary address on the public internet.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        // Shared address space (carrier-grade NAT).
        || (a == 100 && (64..128).contains(&b))
        // IETF protocol assignments.
        || (a == 192 && b == 0 && c == 0)
        // Benchmarking.
        || (a == 198 && (18..20).contains(&b))
        // Reserved.
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let segments = ip.segments();
    // Translation prefixes carry an IPv4 address; judge that instead.
    let embedded = |high: u16, low: u16| Ipv4Addr::from((u32::from(high) << 16) | u32::from(low));
    match segments {
        // NAT64 (64:ff9b::/96).
        [0x64, 0xff9b, 0, 0, 0, 0, high, low] => return is_public_v4(embedded(high, low)),
        // 6to4 (2002::/16).
        [0x2002, high, low, ..] => return is_public_v4(embedded(high, low)),
        _ => {}
    }
    // Only global unicast (2000::/3) can be public. Denying the rest by
    // default also leaves out loopback, unique-local, link-local,
    // multicast, site-local (fec0::/10), local-use NAT64 (64:ff9b:1::/48,
    // which a site's own translator may point at its private IPv4),
    // discard (100::/64), IPv4-compatible (::a.b.c.d) and SRv6 (5f00::/16)
    // addresses, and whatever is assigned later.
    if segments[0] & 0xe000 != 0x2000 {
        return false;
    }
    !(
        // IETF protocol assignments (2001::/23): Teredo, benchmarking,
        // ORCHID and a few anycast services, none of them websites.
        (segments[0] == 0x2001 && segments[1] < 0x0200)
        // Documentation (2001:db8::/32 and 3fff::/20).
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] == 0x3fff && segments[1] < 0x1000)
    )
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// How many names may be looked up at once, by every client together.
/// Each lookup holds a thread of tokio's blocking pool until the system
/// resolver answers, however long a slow name server makes it take, and
/// that pool also reads files, hashes passwords and opens database
/// connections.
const MAX_LOOKUPS: usize = 32;
/// How long a request waits for a name to be looked up.
const LOOKUP_WAIT: Duration = Duration::from_secs(5);

static LOOKUPS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(MAX_LOOKUPS)));

/// DNS that only returns public addresses, looking up at most
/// [`MAX_LOOKUPS`] names at once.
pub struct PublicResolver {
    pub allow_private: bool,
    slots: Arc<Semaphore>,
}

impl PublicResolver {
    pub fn new(allow_private: bool) -> Self {
        Self {
            allow_private,
            slots: Arc::clone(&LOOKUPS),
        }
    }
}

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private = self.allow_private;
        let slots = Arc::clone(&self.slots);
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let addrs: Vec<SocketAddr> = lookup(&slots, &host, LOOKUP_WAIT, system_lookup)
                .await?
                .into_iter()
                .filter(|addr| allow_private || is_public(addr.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} does not resolve to a public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// `host`'s addresses from the system resolver (getaddrinfo), which
/// blocks.
fn system_lookup(host: &str) -> std::io::Result<Vec<SocketAddr>> {
    Ok((host, 0).to_socket_addrs()?.collect())
}

/// Runs `resolve` for `host` on tokio's blocking pool, waiting at most
/// `wait` for it. It holds one of `slots` until it returns, also after
/// the wait ends, since a blocking call can't be cancelled. With no slot
/// free, the lookup is refused at once rather than queued, so a burst
/// leaves no backlog behind.
async fn lookup(
    slots: &Arc<Semaphore>,
    host: &str,
    wait: Duration,
    resolve: fn(&str) -> std::io::Result<Vec<SocketAddr>>,
) -> Result<Vec<SocketAddr>, BoxError> {
    let slot = Arc::clone(slots)
        .try_acquire_owned()
        .map_err(|_| format!("{host} not looked up: too many lookups under way"))?;
    let name = host.to_owned();
    let answer = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        resolve(&name)
    });
    match tokio::time::timeout(wait, answer).await {
        Ok(answer) => Ok(answer??),
        Err(_) => Err(format!("looking up {host} took too long").into()),
    }
}

/// A client whose connections only go to public addresses (unless
/// `allow_private`, for tests against a local server), following
/// redirects as `redirects` says and never through a proxy. It sends no
/// `Referer` on redirects, which would carry the previous URL's query
/// (an API key, say) to the next host.
pub fn client(
    timeout: Duration,
    allow_private: bool,
    redirects: redirect::Policy,
) -> reqwest::Client {
    moekura_storage::install_crypto_provider();
    reqwest::Client::builder()
        .dns_resolver(Arc::new(PublicResolver::new(allow_private)))
        .redirect(redirects)
        .referer(false)
        // A proxy would do the resolving and connecting for us.
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .user_agent(concat!("moekura/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("the HTTP client configuration is valid")
}

/// Whether `host` may be connected to: a name (checked when resolved), or
/// an IP literal that's public (or any, with `allow_private`).
pub fn host_allowed(host: url::Host<&str>, allow_private: bool) -> bool {
    match host {
        url::Host::Domain(_) => true,
        url::Host::Ipv4(v4) => allow_private || is_public(IpAddr::V4(v4)),
        url::Host::Ipv6(v6) => allow_private || is_public(IpAddr::V6(v6)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 0);

    fn slow(_: &str) -> std::io::Result<Vec<SocketAddr>> {
        std::thread::sleep(Duration::from_millis(300));
        Ok(vec![ANSWER])
    }

    #[tokio::test]
    async fn bounds_lookups() {
        let slots = Arc::new(Semaphore::new(1));
        // The caller stops waiting, but the lookup keeps its slot.
        let err = lookup(&slots, "slow.example", Duration::from_millis(20), slow)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("took too long"), "{err}");
        assert_eq!(slots.available_permits(), 0);
        // With none free, the next is refused rather than queued.
        let err = lookup(&slots, "next.example", Duration::from_secs(5), slow)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too many"), "{err}");
        // The slot comes back once the lookup returns.
        for _ in 0..100 {
            if slots.available_permits() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(slots.available_permits(), 1);
        let found = lookup(&slots, "fast.example", Duration::from_secs(5), |_| {
            Ok(vec![ANSWER])
        })
        .await
        .unwrap();
        assert_eq!(found, [ANSWER]);
        assert_eq!(slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn resolves_only_public_addresses() {
        let local = || "localhost".parse::<Name>().unwrap();
        let refused = PublicResolver::new(false).resolve(local()).await;
        let err = refused.err().expect("localhost is refused");
        assert!(err.to_string().contains("public address"), "{err}");
        let allowed = PublicResolver::new(true).resolve(local()).await;
        assert!(allowed.is_ok_and(|mut addrs| addrs.next().is_some()));
    }
}
