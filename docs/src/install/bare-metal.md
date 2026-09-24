# Without containers

You need:

- PostgreSQL 16 or newer, and a database the app owns.
- The media tools, which `serve` and `worker` check for at startup:

| Tool | Used for | Fedora | Debian / Ubuntu |
|---|---|---|---|
| libvips 8.15+ (`vips`, `vipsheader`, `vipsthumbnail`) | reading images, thumbnails, perceptual hashes | `vips-tools` (AVIF: `vips-heif`, JPEG XL: `vips-jxl`) | `libvips-tools libheif-plugin-dav1d libheif-plugin-aomenc` |
| ffmpeg (`ffmpeg`, `ffprobe`) | reading videos, poster frames | `ffmpeg` (RPM Fusion) or `ffmpeg-free` | `ffmpeg` |

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
