# Background jobs

Work that shouldn't hold up a page runs as background jobs, queued in
PostgreSQL:

| Job | Does |
|---|---|
| `media.process` | makes thumbnails, samples and video posters, and the perceptual hash for similar-image search, after an upload |
| `tags.apply_relation` | re-tags existing posts when an alias or implication is approved |
| `posts.purge` | removes a purged post and its files |
| `ml.tag_post` | suggests tags for a post; only [`moekura tagger`](tagger.md) takes these |

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
they can do: `ml.tag_post` jobs wait for a tagger. A failed job is retried
with increasing delays; after its last attempt it's kept as *failed*.
**Admin → Overview** shows the queue, and failed jobs with their errors,
to retry or discard.
