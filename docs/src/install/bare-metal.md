# Without containers

You need:

- PostgreSQL 16 or newer, and a database the app owns (see
  [Requirements](../upgrading.md#requirements) for every version).
- The media tools, which `serve` and `worker` check for at startup:

| Tool | Used for | Fedora | Debian / Ubuntu |
|---|---|---|---|
| libvips 8.15+ (`vips`, `vipsheader`, `vipsthumbnail`) | reading images, thumbnails, perceptual hashes | `vips-tools` (AVIF: `vips-heif`, JPEG XL: `vips-jxl`) | `libvips-tools libheif-plugin-dav1d libheif-plugin-aomenc` |
| ffmpeg 7.0+ (`ffmpeg`, `ffprobe`) | reading videos, poster frames, and videos of ugoira (with libvpx for VP9) | `ffmpeg` (RPM Fusion) or `ffmpeg-free` | `ffmpeg` |

Download a release binary (see [Upgrading](../upgrading.md)), or build one
with Rust 1.94 or newer:

```sh
cargo build --release
sudo install target/release/moekura /usr/local/bin/
```

Create the database:

```sh
sudo -u postgres createuser --pwprompt moekura
sudo -u postgres createdb --owner moekura moekura
```

Then write `/etc/moekura/moekura.toml` (start from
`moekura.example.toml` in the repository) with at least:

```toml
[server]
public_url = "https://booru.example.com"
# Only the reverse proxy on this machine connects.
bind = "127.0.0.1:8080"
trusted_proxies = ["127.0.0.1/32"]

[database]
url = "postgres://moekura:PASSWORD@localhost/moekura"

[storage]
path = "/var/lib/moekura/data"
```

and start it:

```sh
moekura --config /etc/moekura/moekura.toml serve
```

## As a systemd service

```ini
# /etc/systemd/system/moekura.service
[Unit]
Description=Moekura
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
User=moekura
Environment=MOEKURA_CONFIG=/etc/moekura/moekura.toml
# Keeps memory use down after bursts of requests (as in the image).
Environment=MALLOC_ARENA_MAX=2
ExecStart=/usr/local/bin/moekura serve
Restart=on-failure
# Keep the database password out of the config file:
# EnvironmentFile=/etc/moekura/secrets.env

[Install]
WantedBy=multi-user.target
```

```sh
sudo useradd --system --home-dir /var/lib/moekura --create-home moekura
sudo systemctl enable --now moekura
```

`moekura check-config` prints the settings it would use, with secrets
redacted.
