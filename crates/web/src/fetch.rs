//! Downloading uploads from a URL without letting the server be used to
//! reach its own network (SSRF).
//!
//! Every connection goes through [`PublicResolver`], which refuses names
//! resolving to loopback, private, link-local and other non-public
//! addresses. Because the check happens when connecting, a name cannot pass
//! validation and then resolve elsewhere. IP-literal hosts skip DNS, so they
//! are checked in [`check_url`], for the first request and every redirect.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect;
use url::{Host, Url};

use crate::upload::{TempUpload, TempWriter, UploadError};

const MAX_REDIRECTS: usize = 5;

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

/// Refuses URLs we won't fetch: non-web schemes, credentials, and hosts
/// that are non-public IP literals.
pub fn check_url(url: &Url, allow_private: bool) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Only http and https links can be uploaded.".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Links with a user name or password can't be uploaded.".into());
    }
    let ip = match url.host() {
        Some(Host::Ipv4(v4)) => Some(IpAddr::V4(v4)),
        Some(Host::Ipv6(v6)) => Some(IpAddr::V6(v6)),
        Some(Host::Domain(_)) => None,
        None => return Err("That link has no host.".into()),
    };
    if !allow_private && ip.is_some_and(|ip| !is_public(ip)) {
        return Err("That address isn't on the public internet.".into());
    }
    Ok(())
}

/// DNS that only returns public addresses.
struct PublicResolver {
    allow_private: bool,
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

#[derive(Clone)]
pub struct Fetcher {
    client: reqwest::Client,
    allow_private: bool,
}

impl Fetcher {
    /// `allow_private` exists for tests against a local server; the app
    /// always passes false.
    pub fn new(timeout: Duration, allow_private: bool) -> Self {
        moekura_storage::install_crypto_provider();
        let policy = redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            match check_url(attempt.url(), allow_private) {
                Ok(()) => attempt.follow(),
                Err(reason) => attempt.error(reason),
            }
        });
        let client = reqwest::Client::builder()
            .dns_resolver(Arc::new(PublicResolver { allow_private }))
            .redirect(policy)
            // A proxy would do the resolving and connecting for us.
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .timeout(timeout)
            .user_agent(concat!("moekura/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("the HTTP client configuration is valid");
        Self {
            client,
            allow_private,
        }
    }

    /// Downloads `url` into a temporary upload, at most `limit` bytes.
    pub async fn fetch(
        &self,
        url: &Url,
        writer: TempWriter,
        limit: u64,
    ) -> Result<TempUpload, UploadError> {
        check_url(url, self.allow_private).map_err(UploadError::Invalid)?;
        let unreachable = |e: reqwest::Error| {
            tracing::info!(%url, error = %e, "URL upload failed");
            UploadError::Invalid(format!(
                "Couldn't download that link ({}).",
                short_reason(&e)
            ))
        };
        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(unreachable)?;
        if !response.status().is_success() {
            return Err(UploadError::Invalid(format!(
                "That link returned {}.",
                response.status()
            )));
        }
        let too_large = || {
            UploadError::Invalid(format!(
                "The file is larger than {} MB.",
                limit / (1024 * 1024)
            ))
        };
        if response.content_length().is_some_and(|len| len > limit) {
            return Err(too_large());
        }
        let mut writer = writer;
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(unreachable)?;
            if writer.written() + chunk.len() as u64 > limit {
                return Err(too_large());
            }
            writer.write(&chunk).await?;
        }
        writer.finish().await
    }
}

fn short_reason(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "it took too long"
    } else if error.is_redirect() {
        "it redirected somewhere not allowed"
    } else if error.is_connect() {
        "couldn't connect"
    } else {
        "the request failed"
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::response::Redirect;
    use axum::routing::get;

    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn classifies_addresses() {
        for public in [
            "8.8.8.8",
            "1.1.1.1",
            "151.101.1.69",
            "2606:4700::1111",
            "2a00:1450:4001::200e",
        ] {
            assert!(is_public(ip(public)), "{public}");
        }
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "192.0.0.8",
            "198.18.0.1",
            "203.0.113.5",
            "224.0.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:7f00:1::",
            "2001:0::1",
            "::127.0.0.1",
        ] {
            assert!(!is_public(ip(private)), "{private}");
        }
        // Translated public addresses stay public.
        assert!(is_public(ip("::ffff:8.8.8.8")));
        assert!(is_public(ip("64:ff9b::808:808")));
    }

    #[test]
    fn checks_urls() {
        let check = |s: &str| check_url(&Url::parse(s).unwrap(), false);
        assert!(check("https://example.com/a.png").is_ok());
        assert!(check("http://8.8.8.8/a.png").is_ok());
        for bad in [
            "ftp://example.com/a",
            "file:///etc/passwd",
            "http://127.0.0.1/",
            "http://[::1]:8080/",
            "http://169.254.169.254/latest/meta-data/",
            "http://user:pass@example.com/",
            "http://0x7f000001/",
        ] {
            assert!(check(bad).is_err(), "{bad}");
        }
    }

    /// A local server with an image, a redirect to it, and a large file.
    async fn serve_fixtures() -> SocketAddr {
        let png = crate::test_support::fixture::png(20, 20);
        let app = Router::new()
            .route("/a.png", get(move || async move { png }))
            .route("/redirect", get(|| async { Redirect::temporary("/a.png") }))
            .route("/big", get(|| async { vec![0u8; 2 * 1024 * 1024] }))
            .route(
                "/missing",
                get(|| async { axum::http::StatusCode::NOT_FOUND }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        addr
    }

    async fn fetch(fetcher: &Fetcher, url: &str) -> Result<TempUpload, UploadError> {
        let dir = std::env::temp_dir().join(format!("moekura-fetch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let writer = TempWriter::create(&dir).await.unwrap();
        fetcher
            .fetch(&Url::parse(url).unwrap(), writer, 1024 * 1024)
            .await
    }

    #[tokio::test]
    async fn downloads_and_hashes() {
        let addr = serve_fixtures().await;
        let fetcher = Fetcher::new(Duration::from_secs(10), true);
        let upload = fetch(&fetcher, &format!("http://{addr}/redirect"))
            .await
            .unwrap();
        let bytes = std::fs::read(upload.path()).unwrap();
        assert_eq!(bytes, crate::test_support::fixture::png(20, 20));
        assert_eq!(upload.size, bytes.len() as u64);
    }

    #[tokio::test]
    async fn refuses_local_addresses_even_via_dns() {
        let addr = serve_fixtures().await;
        let fetcher = Fetcher::new(Duration::from_secs(10), false);
        // IP literal: refused before connecting.
        let err = fetch(&fetcher, &format!("http://{addr}/a.png"))
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("public internet"), "{err}");
        // A name resolving to loopback: refused by the resolver.
        let err = fetch(&fetcher, &format!("http://localhost:{}/a.png", addr.port()))
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("Couldn't download"), "{err}");
    }

    #[tokio::test]
    async fn enforces_size_and_status() {
        let addr = serve_fixtures().await;
        let fetcher = Fetcher::new(Duration::from_secs(10), true);
        let err = fetch(&fetcher, &format!("http://{addr}/big"))
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("larger than 1 MB"), "{err}");
        let err = fetch(&fetcher, &format!("http://{addr}/missing"))
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("404"), "{err}");
    }
}
