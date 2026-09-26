//! Outbound HTTP that can't be used to reach the server's own network
//! (SSRF): for fetching uploads from links and delivering webhooks.
//!
//! Every connection goes through a resolver that refuses names resolving
//! to loopback, private, link-local and other non-public addresses.
//! Because the check happens when connecting, a name can't pass
//! validation and then resolve elsewhere. IP-literal hosts skip DNS, so
//! callers check them with [`is_public`] for the first request and every
//! redirect.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect;

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
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        // Documentation (2001:db8::/32) and Teredo (2001::/32).
        || (segments[0] == 0x2001 && (segments[1] == 0x0db8 || segments[1] == 0))
        // Deprecated IPv4-compatible addresses (::a.b.c.d).
        || segments[..6] == [0; 6])
}

/// DNS that only returns public addresses.
pub struct PublicResolver {
    pub allow_private: bool,
}

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_private = self.allow_private;
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|addr| allow_private || is_public(addr.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} does not resolve to a public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// A client whose connections only go to public addresses (unless
/// `allow_private`, for tests against a local server), following
/// redirects as `redirects` says and never through a proxy.
pub fn client(
    timeout: Duration,
    allow_private: bool,
    redirects: redirect::Policy,
) -> reqwest::Client {
    moekura_storage::install_crypto_provider();
    reqwest::Client::builder()
        .dns_resolver(Arc::new(PublicResolver { allow_private }))
        .redirect(redirects)
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
