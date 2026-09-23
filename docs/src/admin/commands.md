# Commands

| Command | Does |
|---|---|
| `uwubooru serve` | runs the web server, plus job workers unless `jobs.run_in_serve = false`; migrates first unless `database.auto_migrate = false` |
| `uwubooru worker` | runs job workers only |
| `uwubooru migrate` | applies pending migrations and exits |
| `uwubooru check-config` | validates the configuration and prints it, secrets redacted |
| `uwubooru openapi` | prints the API's OpenAPI description |
| `uwubooru admin create-user NAME [--role ROLE] [--email E]` | creates an account; asks for the password, or reads one line from standard input |
| `uwubooru admin set-role NAME ROLE` | changes someone's role |
| `uwubooru admin create-invite [--uses N] [--expires-days D]` | makes an invite code, shown once |
| `uwubooru admin settings` | shows the site settings |
| `uwubooru admin settings set KEY VALUE` | changes one; `VALUE` is JSON, or else a plain string |
| `uwubooru admin regenerate-media (--all \| IDS…)` | remakes thumbnails and samples |
| `uwubooru admin recount-tags` | recomputes every tag's post count |

Every command takes `--config PATH`. Role and setting changes made here
appear in the moderation log.
