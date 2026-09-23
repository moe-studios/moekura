//! Working out the client's IP address behind reverse proxies.

use std::net::{IpAddr, SocketAddr};

use axum::extract::ConnectInfo;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::HeaderMap;
use axum::http::request::Parts;
use ipnet::IpNet;

/// The client's address: the connection's peer, unless the peer is a
/// trusted proxy, in which case `X-Forwarded-For` is walked from the right
/// (the hops our proxies appended) to the first untrusted address.
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
    Some(resolve(peer, &parts.headers, trusted_proxies))
}

fn resolve(peer: IpAddr, headers: &HeaderMap, trusted_proxies: &[IpNet]) -> IpAddr {
    let trusted = |ip: &IpAddr| trusted_proxies.iter().any(|net| net.contains(ip));
    if !trusted(&peer) {
        return peer;
    }
    let hops: Vec<IpAddr> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map_while(|hop| hop.trim().parse().ok())
        .collect();
    // Nearest hop first. Everything left of the first untrusted address was
    // written by the client and can't be believed.
    hops.into_iter()
        .rev()
        .find(|ip| !trusted(ip))
        .unwrap_or(peer)
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
}
