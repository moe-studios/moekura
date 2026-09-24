# Commands

| Command | Does |
|---|---|
| `moekura serve` | runs the web server, plus job workers unless `jobs.run_in_serve = false`; migrates first unless `database.auto_migrate = false` |
| `moekura worker` | runs job workers only |
| `moekura migrate` | applies pending migrations and exits |
| `moekura check-config` | validates the configuration and prints it, secrets redacted |
| `moekura openapi` | prints the API's OpenAPI description |
| `moekura admin create-user NAME [--role ROLE] [--email E]` | creates an account; asks for the password, or reads one line from standard input |
| `moekura admin set-role NAME ROLE` | changes someone's role |
| `moekura admin create-invite [--uses N] [--expires-days D]` | makes an invite code, shown once |
| `moekura admin settings` | shows the site settings |
| `moekura admin settings set KEY VALUE` | changes one; `VALUE` is JSON, or else a plain string |
| `moekura admin regenerate-media (--all \| IDS…)` | remakes thumbnails and samples |
| `moekura admin recount-tags` | recomputes every tag's post count |
| `moekura admin send-test-mail ADDRESS` | sends a test message through the [`[mail]`](../configuration.md#mail) settings |
| `moekura admin import DIR --uploader NAME …` | imports a folder of files; see [Bulk import](import.md) |
| `moekura admin seed --posts N [--tags T] [--seed S]` | fills a test database with synthetic posts for load testing; see [Scaling](../scaling.md) |

Every command takes `--config PATH`. Role and setting changes made here
appear in the moderation log.
