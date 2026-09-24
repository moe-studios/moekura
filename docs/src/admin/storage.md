# File storage

Originals, thumbnails, samples and video posters are stored under keys
derived from the file's SHA-256, such as
`original/ab/cd/abcd….png`. The same file is only ever stored once.

## On disk

The default. Files go under `storage.path` (`./data`, or
`/var/lib/uwubooru/data` in the container image), and the app serves them
under `/data/` with long cache lifetimes: a key never changes content.

## S3-compatible storage

Any S3-compatible store works: AWS S3, MinIO, Garage, SeaweedFS, Cloudflare
R2, Backblaze B2.

```toml
[storage]
backend = "s3"

[storage.s3]
bucket = "uwubooru"
region = "auto"
endpoint = "https://ACCOUNT.r2.cloudflarestorage.com"
# Or UWU_STORAGE__S3__ACCESS_KEY_ID / UWU_STORAGE__S3__SECRET_ACCESS_KEY.
access_key_id = "…"
secret_access_key = "…"
# Most self-hosted stores need this.
path_style = true
```

Objects are stored with their content type and
`Cache-Control: public, max-age=31536000, immutable`: a key never changes
content, so a CDN or browser may keep it forever.

Without `public_base_url`, the app streams files from the bucket, which can
stay private.

## A CDN or public bucket

Set `storage.public_base_url` to where browsers can load the files, and the
app links there instead of serving them:

```toml
[storage]
public_base_url = "https://cdn.example.com"
```

The site's content security policy allows images and videos from that
origin. Don't do this on a [private site](private-sites.md).

For a CDN in front of a bucket:

- make the bucket (or the path the CDN reads) publicly readable, or give
  the CDN its own credentials;
- let the CDN honour the objects' `Cache-Control`, or cache everything
  for a long time: nothing under a key ever changes;
- serve the files from a different domain than the site (for example
  `cdn.example.com`), so uploaded files can never run as the site.

Files stored by versions before 0.1 have no content type or caching
headers of their own. `uwubooru admin regenerate-media --all` stores the
thumbnails and samples again; the originals keep what they had, so give the
CDN a default `Cache-Control`.

## Media settings

After changing thumbnail sizes, the sample size or the format under
`[media]`, regenerate what's stored:

```sh
uwubooru admin regenerate-media --all
```
