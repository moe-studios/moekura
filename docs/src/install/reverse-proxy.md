# Behind a reverse proxy

Put a reverse proxy in front of the app for HTTPS. Then tell the app two
things:

- `server.public_url`: the `https://` address people use. Cookies are then
  marked `Secure`, and forms are only accepted from that origin.
- `server.trusted_proxies`: the proxy's address. The app believes the
  `X-Forwarded-For` header only from these, and uses it for bans and rate
  limits. Without it, every visitor appears to come from the proxy.

```toml
[server]
public_url = "https://booru.example.com"
trusted_proxies = ["127.0.0.1/32"]
```

The proxy must pass the `Host` and `Origin` headers through unchanged:
they protect forms against cross-site requests.

## Caddy

```text
booru.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

Caddy gets a certificate, sets `X-Forwarded-For`, and has no upload size
limit by default.

## nginx

```nginx
server {
    listen 443 ssl;
    server_name booru.example.com;
    # ssl_certificate …

    # At least media.max_upload_mb.
    client_max_body_size 110m;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        # Stream uploads to the app instead of buffering them.
        proxy_request_buffering off;
    }
}
```

## Health checks

`GET /healthz` answers when the process is up; `GET /readyz` also checks
the database. Point load balancers at `/readyz`.
