# Without containers

You need:

- PostgreSQL 16 or newer, and a database the app owns.
- The media tools, which `serve` and `worker` check for at startup:

| Tool | Used for | Fedora | Debian / Ubuntu |
|---|---|---|---|
| libvips 8.15+ (`vips`, `vipsheader`, `vipsthumbnail`) | reading images, thumbnails, perceptual hashes | `vips-tools` (AVIF: `vips-heif`, JPEG XL: `vips-jxl`) | `libvips-tools libheif-plugin-dav1d libheif-plugin-aomenc` |
| ffmpeg (`ffmpeg`, `ffprobe`) | reading videos, poster frames | `ffmpeg` (RPM Fusion) or `ffmpeg-free` | `ffmpeg` |

Build the binary with Rust 1.94 or newer:

```sh
cargo build --release
sudo install target/release/uwubooru /usr/local/bin/
```

Create the database:

```sh
sudo -u postgres createuser --pwprompt uwu
sudo -u postgres createdb --owner uwu uwu
```

Then write `/etc/uwubooru/uwubooru.toml` (start from
`uwubooru.example.toml` in the repository) with at least:

```toml
[server]
public_url = "https://booru.example.com"

[database]
url = "postgres://uwu:PASSWORD@localhost/uwu"

[storage]
path = "/var/lib/uwubooru/data"
```

and start it:

```sh
uwubooru --config /etc/uwubooru/uwubooru.toml serve
```

## As a systemd service

```ini
# /etc/systemd/system/uwubooru.service
[Unit]
Description=uwubooru
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
User=uwubooru
Environment=UWU_CONFIG=/etc/uwubooru/uwubooru.toml
ExecStart=/usr/local/bin/uwubooru serve
Restart=on-failure
# Keep the database password out of the config file:
# EnvironmentFile=/etc/uwubooru/secrets.env

[Install]
WantedBy=multi-user.target
```

```sh
sudo useradd --system --home-dir /var/lib/uwubooru --create-home uwubooru
sudo systemctl enable --now uwubooru
```

`uwubooru check-config` prints the settings it would use, with secrets
redacted.
