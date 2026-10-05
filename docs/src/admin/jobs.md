# Background jobs

Work that shouldn't hold up a page runs as background jobs, queued in
PostgreSQL:

| Job | Does |
|---|---|
| `media.process` | makes thumbnails, samples and video posters, and the perceptual hash for similar-image search, after an upload |
| `media.hash_pixels` | once, after the upgrade that added pixel hashes: hashes the pixels of earlier posts, for pixel-perfect duplicate warnings |
| `tags.apply_relation` | re-tags existing posts when an alias or implication is approved |
| `posts.purge` | removes a purged post and its files |
| `ml.tag_post` | suggests tags for a post; only [`moekura tagger`](tagger.md) takes these |
| `ml.tag_staged` | suggests tags for an uploaded file's post form, before it's posted; also only for the tagger |
| `stats.refresh` | hourly, counts the figures for [stats and reports](moderation.md#stats-and-reports) |

`serve` runs job workers itself, `jobs.workers` at a time. On a busier site,
run workers as separate processes and turn them off in the web servers:

```sh
moekura worker          # as many as you like, on any machine
```

```toml
[jobs]
run_in_serve = false     # on the web servers
```

Workers claim jobs without stepping on each other, and only the kinds
they can do: `ml.tag_post` jobs wait for a tagger. Webhook deliveries,
which wait on other servers, run at most `webhooks.max_concurrent` at a
time across all workers, so they never hold every worker. A failed job is retried
with increasing delays; after its last attempt it's kept as *failed*.
**Admin → Overview** shows the queue, and failed jobs with their errors,
to retry or discard. Errors and the buttons are only for those who
manage site settings (an error can quote what the job was sending to),
and a failed mail shows only who it was for, never its text, which may
hold a password reset link.
