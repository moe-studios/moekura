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

    # At least media.max_upload_mb, for forms sent without scripts (the
    # upload page sends files in pieces of media.upload_chunk_mb).
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

## Large uploads behind a CDN

CDNs and some proxies limit how large one request can be: Cloudflare
allows 100 MB on its Free and Pro plans (200 MB on Business, 500 MB on
Enterprise), and nginx 1 MB unless `client_max_body_size` says otherwise.
The upload page doesn't send a file in one request: it sends each file in
pieces of at most `media.upload_chunk_mb` (50 MB by default), each in a
request of its own, then the form naming the files. So the CDN's limit
caps the pieces, not the files, and `media.max_upload_mb` can be larger
than it. Keep `upload_chunk_mb` below the limit:

```toml
[media]
max_upload_mb = 500
# Below Cloudflare's 100 MB.
upload_chunk_mb = 50
```

The page sizes pieces to take about ten seconds on the uploader's
connection, so each request also ends well within
`server.request_timeout_secs` and the CDN's own timeouts. A piece that
fails (a dropped connection, a timeout, a proxy refusing it as too large)
is sent again, smaller, from where the server says the file got to.

The pieces wait in [storage](../admin/storage.md), under `transfer/`,
until the form naming the file is sent; then they're put together and
removed. With several web servers, any of them can take each piece. A
file no piece came for in an hour is removed, with its pieces. Each user
can be sending at most 40 files at once. Pieces are never served under
`/data/`; with a public bucket, their names are unguessable and they live
only minutes, but keep the `transfer/` prefix private if your bucket lets
you.

Scripts can send files the same way (see [the API](../api.md#large-files)).
What still goes in one request, so the CDN's limit still applies: the
upload form without scripts, a `file` sent to `POST /upload` or the API,
replacing a post's file, searching by image, and uploads through the
[Danbooru-compatible API](../using/danbooru-clients.md).

## Health checks

`GET /healthz` answers when the process is up; `GET /readyz` also checks
the database. Point load balancers at `/readyz`.
