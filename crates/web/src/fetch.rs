//! Downloading uploads from a URL without letting the server be used to
//! reach its own network (SSRF), through [`moekura_net`]. IP-literal hosts
//! skip DNS, so they are checked in [`check_url`], for the first request
//! and every redirect.

use std::net::IpAddr;
use std::time::Duration;

use futures_util::StreamExt;
pub use moekura_net::is_public;
use reqwest::redirect;
use url::{Host, Url};

use crate::upload::{TempUpload, TempWriter, UploadError};

const MAX_REDIRECTS: usize = 5;

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

#[derive(Clone)]
pub struct Fetcher {
    client: reqwest::Client,
    allow_private: bool,
}

impl Fetcher {
    /// `allow_private` exists for tests against a local server; the app
    /// always passes false.
    pub fn new(timeout: Duration, allow_private: bool) -> Self {
        let policy = redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            match check_url(attempt.url(), allow_private) {
                Ok(()) => attempt.follow(),
                Err(reason) => attempt.error(reason),
            }
        });
        let client = moekura_net::client(timeout, allow_private, policy);
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
    use std::net::SocketAddr;

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
