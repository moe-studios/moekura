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

The address to list is the one the app sees the proxy connect from,
which isn't always `127.0.0.1`: with [Docker Compose](compose.md), a
proxy on the host reaches the app from the compose network's gateway
(see [below](#with-docker-compose)). If the header comes from an
address that isn't listed, the app ignores it and logs a warning naming
that address, once after each start. IPv4 addresses are compared as
IPv4, also when the app listens on IPv6 as well (`[::]`), so
`127.0.0.1/32` and `::ffff:127.0.0.1/128` mean the same.

Trust only your proxies' addresses, and keep the app's port unreachable
from elsewhere, or anyone who can reach it directly can claim any
address.

The app reads `X-Forwarded-For` from the right: it skips the entries
your trusted proxies added and takes the first address before them.
Entries further left were written by the client and are ignored, and so
is everything left of an entry that isn't an address. Both appending the
client's address (nginx's `$proxy_add_x_forwarded_for`) and replacing
the header (Caddy) work.

The proxy must pass the `Host` and `Origin` headers through unchanged:
they protect forms against cross-site requests.

Let only the proxy reach the app: bind it to `127.0.0.1:8080` when the
proxy runs on the same machine (the compose file publishes its port
there), or keep the port behind a firewall. The app has its own
[connection limits](../configuration.md#serverconnections), but it
can't tell clients apart behind them; the proxy's client timeouts and
per-client limits (nginx's `limit_conn`) can.

## With Docker Compose

Docker picks the compose network's addresses when it creates the
network, so its gateway, the address a proxy on the host connects from,
can change when the network is recreated, and the proxy would quietly
stop being trusted. Pin the network in `deploy/compose.tiny.yml`, trust
its gateway, and publish the app's port on the host's loopback only
(published on every interface, outside connections would also arrive
from the gateway, and be trusted):

```yaml
services:
  app:
    ports:
      - "127.0.0.1:8080:8080"
    environment:
      MOEKURA_SERVER__TRUSTED_PROXIES: '["172.30.0.1/32"]'
      # …

networks:
  default:
    ipam:
      config:
        - subnet: 172.30.0.0/24
          gateway: 172.30.0.1
```

Pick a subnet no other network on the host uses. If the warning about an
untrusted address appears in the log anyway, list the address it names.

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
