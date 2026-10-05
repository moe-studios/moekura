//! Working out the client's IP address behind reverse proxies.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::ConnectInfo;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::HeaderMap;
use axum::http::request::Parts;
use ipnet::IpNet;

/// The client's address: the connection's peer, unless the peer is a
/// trusted proxy, in which case `X-Forwarded-For` is walked from the right
/// (the hops our proxies appended) to the first untrusted address.
///
/// Always canonical: an IPv4 client of a dual-stack listener is
/// `192.0.2.1`, not `::ffff:192.0.2.1`, so IPv4 bans and limits apply.
pub fn client_ip(parts: &Parts, trusted_proxies: &[IpNet]) -> Option<IpAddr> {
    // Same lookup as axum's ConnectInfo extractor, including its test mock.
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
        .or_else(|| {
            parts
                .extensions
                .get::<MockConnectInfo<SocketAddr>>()
                .map(|MockConnectInfo(addr)| addr.ip())
        })?;
    Some(resolve(
        peer.to_canonical(),
        &parts.headers,
        trusted_proxies,
    ))
}

/// Set once `X-Forwarded-For` has come from a peer that isn't trusted.
static WARNED_UNTRUSTED: AtomicBool = AtomicBool::new(false);

fn resolve(peer: IpAddr, headers: &HeaderMap, trusted_proxies: &[IpNet]) -> IpAddr {
    let trusted = |ip: &IpAddr| trusted_proxies.iter().any(|net| net.contains(ip));
    if !trusted(&peer) {
        // Most likely a proxy missing from the list, which makes every
        // visitor share its address for bans and limits.
        if headers.contains_key("x-forwarded-for")
            && !WARNED_UNTRUSTED.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                %peer,
                "ignored X-Forwarded-For from an address not in server.trusted_proxies; \
                 if it is your reverse proxy, every visitor appears to come from it"
            );
        }
        return peer;
    }
    // Nearest hop first: the last header's last entry. Each is split as
    // bytes, so a bad byte or entry the client wrote can't hide the hops
    // our proxies appended after it.
    let hops = headers
        .get_all("x-forwarded-for")
        .iter()
        .rev()
        .flat_map(|value| value.as_bytes().rsplit(|&b| b == b','));
    for hop in hops {
        let ip = std::str::from_utf8(hop)
            .ok()
            .and_then(|hop| hop.trim().parse::<IpAddr>().ok());
        match ip.map(|ip| ip.to_canonical()) {
            Some(ip) if trusted(&ip) => {}
            Some(ip) => return ip,
            // Not something a trusted proxy writes: nothing from here
            // leftwards can be believed.
            None => break,
        }
    }
    peer
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(xff: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in xff {
            map.append("x-forwarded-for", HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn nets(list: &[&str]) -> Vec<IpNet> {
        list.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn untrusted_peers_cannot_spoof() {
        let result = resolve(
            ip("203.0.113.7"),
            &headers(&["1.2.3.4"]),
            &nets(&["10.0.0.0/8"]),
        );
        assert_eq!(result, ip("203.0.113.7"));
        // And the operator is told the header was ignored.
        assert!(WARNED_UNTRUSTED.load(Ordering::Relaxed));
    }

    #[test]
    fn trusted_proxy_reports_the_client() {
        let result = resolve(
            ip("10.0.0.2"),
            &headers(&["198.51.100.9"]),
            &nets(&["10.0.0.0/8"]),
        );
        assert_eq!(result, ip("198.51.100.9"));
    }

    #[test]
    fn skips_trusted_hops_and_ignores_client_supplied_entries() {
        // Client sent a fake "1.2.3.4"; our CDN (10.1.1.1) and LB appended real hops.
        let chain = headers(&["1.2.3.4, 198.51.100.9", "10.1.1.1"]);
        let result = resolve(ip("10.0.0.2"), &chain, &nets(&["10.0.0.0/8"]));
        assert_eq!(result, ip("198.51.100.9"));
    }

    #[test]
    fn client_written_junk_cannot_hide_the_appended_hop() {
        let trusted = nets(&["10.0.0.0/8"]);
        let proxy = ip("10.0.0.2");
        // The client sends a chosen address and something that isn't
        // one; an appending proxy adds the real address after them.
        for sent in ["1.2.3.4, junk", "1.2.3.4,", "1.2.3.4, , junk", "junk"] {
            let chain = headers(&[&format!("{sent}, 198.51.100.9")]);
            assert_eq!(
                resolve(proxy, &chain, &trusted),
                ip("198.51.100.9"),
                "{sent}"
            );
        }
        // Also in a header of its own, before the proxy's.
        let chain = headers(&["1.2.3.4, junk", "198.51.100.9"]);
        assert_eq!(resolve(proxy, &chain, &trusted), ip("198.51.100.9"));
        // A byte that isn't UTF-8 spoils only its own entry.
        let mut chain = HeaderMap::new();
        chain.append(
            "x-forwarded-for",
            HeaderValue::from_bytes(b"1.2.3.4, \xff, 198.51.100.9").unwrap(),
        );
        assert_eq!(resolve(proxy, &chain, &trusted), ip("198.51.100.9"));
        // Junk right of the client is where trust ends: the walk stops
        // there instead of reading on to the spoofed address.
        let chain = headers(&["1.2.3.4, junk, 10.1.1.1"]);
        assert_eq!(resolve(proxy, &chain, &trusted), proxy);
    }

    #[test]
    fn falls_back_to_the_peer() {
        let trusted = nets(&["10.0.0.0/8"]);
        assert_eq!(
            resolve(ip("10.0.0.2"), &headers(&[]), &trusted),
            ip("10.0.0.2")
        );
        assert_eq!(
            resolve(ip("10.0.0.2"), &headers(&["garbage"]), &trusted),
            ip("10.0.0.2")
        );
        assert_eq!(
            resolve(ip("10.0.0.2"), &headers(&["10.9.9.9"]), &trusted),
            ip("10.0.0.2")
        );
    }

    #[test]
    fn handles_ipv6() {
        let result = resolve(ip("::1"), &headers(&["2001:db8::5"]), &nets(&["::1/128"]));
        assert_eq!(result, ip("2001:db8::5"));
    }

    #[test]
    fn ipv4_mapped_addresses_count_as_ipv4() {
        let parts = |peer: &str, xff: &[&str]| {
            let mut request = axum::http::Request::new(());
            *request.headers_mut() = headers(xff);
            request
                .extensions_mut()
                .insert(MockConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            request.into_parts().0
        };
        // A dual-stack listener sees IPv4 clients as ::ffff:a.b.c.d.
        assert_eq!(
            client_ip(&parts("[::ffff:198.51.100.7]:4000", &[]), &[]),
            Some(ip("198.51.100.7"))
        );
        // Trusted proxies are matched as IPv4, and so are the hops.
        let trusted = nets(&["10.0.0.0/8"]);
        assert_eq!(
            client_ip(
                &parts("[::ffff:10.0.0.2]:4000", &["::ffff:198.51.100.9"]),
                &trusted
            ),
            Some(ip("198.51.100.9"))
        );
        assert_eq!(
            client_ip(
                &parts("[::ffff:10.0.0.2]:4000", &["198.51.100.9, ::ffff:10.1.1.1"]),
                &trusted
            ),
            Some(ip("198.51.100.9"))
        );
    }
}
