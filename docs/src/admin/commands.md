# Commands

| Command | Does |
|---|---|
| `moekura serve` | runs the web server, plus job workers unless `jobs.run_in_serve = false`; migrates first unless `database.auto_migrate = false` |
| `moekura worker` | runs job workers only |
| `moekura tagger [--check]` | runs the [tagger](tagger.md) (a build with the `tagger` feature); `--check` only loads ONNX Runtime |
| `moekura migrate` | applies pending migrations and exits |
| `moekura check-config` | validates the configuration and prints it, secrets redacted |
| `moekura openapi` | prints the API's OpenAPI description |
| `moekura admin create-user NAME [--role ROLE] [--email E]` | creates an account; asks for the password, or reads one line from standard input |
| `moekura admin set-role NAME ROLE` | changes someone's role |
| `moekura admin lift-network-ban ADDRESS_OR_RANGE` | lifts the [network bans](moderation.md#bans) covering an address, or overlapping a range, such as one that locks staff out |
| `moekura admin create-invite [--uses N] [--expires-days D]` | makes an invite code, shown once |
| `moekura admin settings` | shows the site settings |
| `moekura admin settings set KEY VALUE` | changes one; `VALUE` is JSON, or else a plain string |
| `moekura admin regenerate-media (--all \| IDS…)` | remakes thumbnails and samples, and rereads the files' metadata |
| `moekura admin recount-tags` | recomputes every tag's post count |
| `moekura admin tag-backlog [--all] [--limit N]` | queues posts the [tagger](tagger.md) hasn't seen (or, with `--all`, every post) |
| `moekura admin send-test-mail ADDRESS` | sends a test message through the [`[mail]`](../configuration.md#mail) settings |
| `moekura admin import DIR --uploader NAME …` | imports a folder of files; see [Bulk import](import.md) |
| `moekura admin export DIR [--tags SEARCH] [--include-deleted]` | writes posts' files and sidecars to a folder; see [Exporting](import.md#exporting) |
| `moekura admin seed --posts N [--tags T] [--seed S]` | fills a test database with synthetic posts for load testing; see [Scaling](../scaling.md) |
| `moekura admin bench [--check] [--explain NAME]` | times a suite of searches against the database |
| `moekura admin bench-http [--url URL] [--concurrency N] [--check-ms MS]` | times pages and API responses from a running server; see [Scaling](../scaling.md) |

Every command takes `--config PATH`. Role and setting changes, and
lifted network bans, made here appear in the moderation log.
