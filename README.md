# Moekura

A self-hostable booru (tag-based image board) that scales from a private
single-user instance to a public site with millions of posts, using the same
binary and schema at every size.

The name is *moe* (萌え) + *kura* (蔵, "storehouse" or "warehouse"): a
storehouse for the things you love.

> **Status: early development.** Uploads, tags and search, the post pages,
> moderation, the admin panel and the API work; see
> [docs/design.md](docs/design.md) for the plan and roadmap.

## Documentation

The [book in `docs/`](docs/src/SUMMARY.md) covers installing, configuring,
moderating and using a site, and the API. Build it with
[mdBook](https://rust-lang.github.io/mdBook/): `mdbook serve docs`.

Some starting points:

- [Installing with Docker Compose](docs/src/install/compose.md) or
  [without containers](docs/src/install/bare-metal.md)
- [Configuration](docs/src/configuration.md)
- [Search syntax](docs/src/using/search.md)
- [The API](docs/src/api.md); each site also serves its own reference at
  `/api/docs`. Apps made for Danbooru work too:
  [Danbooru apps](docs/src/using/danbooru-clients.md)
- [Development](docs/src/development.md)

## Quick start

```sh
echo "POSTGRES_PASSWORD=$(openssl rand -hex 16)" > deploy/.env
docker compose -f deploy/compose.tiny.yml up -d
docker compose -f deploy/compose.tiny.yml exec app moekura admin create-user yourname --role admin
```

Then open <http://localhost:8080> and log in.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Please report security problems
privately, as described in [SECURITY.md](SECURITY.md).

## License

[AGPL-3.0-only](LICENSE). If you run a modified version as a public
service, you must publish your changes.
