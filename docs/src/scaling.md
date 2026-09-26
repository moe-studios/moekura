# Scaling

A single `moekura serve` with PostgreSQL on the same machine is enough for
a private collection or a small community. As a site grows, the same
program scales out:

- run web servers behind a load balancer; they keep no state of their own;
- move background jobs to [separate `moekura worker` processes](admin/jobs.md);
- add PostgreSQL read replicas (`database.replicas`), which take searches
  and listings;
- store files in [S3-compatible storage](admin/storage.md) with a CDN in
  front;
- with several web servers, point them at a shared Valkey
  (`cache.backend = "valkey"`), so login and registration limits count
  across all of them;
- with several web servers, set `database.auto_migrate = false` and run
  `moekura migrate` when deploying.

## Read replicas

List PostgreSQL streaming replicas in `database.replicas`:

```toml
[database]
url = "postgres://moekura:…@primary/moekura"
replicas = ["postgres://moekura:…@replica-1/moekura", "postgres://moekura:…@replica-2/moekura"]
```

Searches, listings, tag pages, profiles and history then read from the
replicas in turn; everything else, and every change, uses the primary.
Every few seconds each server checks its replicas: one that doesn't answer,
or is more than `database.replica_max_lag_secs` behind, is skipped until
it's back (the log says when). With no usable replica, reads go to the
primary.

After someone changes something (an upload, an edit, a favorite), their
own reads go to the primary for `replica_max_lag_secs`, so they always see
what they just did. Other people may see it a moment later.

## How fast is search?

Measured with 5,000,000 synthetic posts (250,000 tags, with 1.8 million
comments, 530,000 notes, 12,500 pools and 14 million tagger suggestions;
an 11 GB database) on a 16-core machine with 32 GB of memory, PostgreSQL 18
given `shared_buffers = 4GB`. Each search is what a results page runs:
looking up the tags, fetching the page, and counting. Times are the 95th
percentile of 20 runs, in milliseconds:

| Search | Example | ms |
|---|---|---|
| front page | | 0.3 |
| a tag on 70% of posts | `red_red` | 0.7 |
| two / three common tags | `red_red blue_red` | 8 / 15 |
| a common tag without another | `blue_red -red_red` | 19 |
| a rare tag (200 posts) | `detailed_back_3` | 1.2 |
| a common and a rare tag | `red_red detailed_back_3` | 8.7 |
| either of two tags | `~wet_red ~dry_red` | 30 |
| wildcards | `wet_*`, `*_red` | 38, 6 |
| file type and a tag | `filetype:mp4 blue_red` | 53 |
| rating and score | `rating:e score:>20` | 1.6 |
| a year, with a tag | `date:2021`, `blue_red date:2021` | 2.4, 5.4 |
| page 500 of a common tag | | 6.5 |
| a cursor deep into a common tag | `page=b2500000` | 0.6 |
| a pool, in its own order | `pool:23`, `ordpool:23` | 1.8, 1.6 |
| posts in any pool | `pool:any` | 41 |
| a favorite group (300 posts) | `favgroup:1235` | 2 |
| a word in notes, common / rare | `note:red`, `note:new` | 74 / 23 |
| recently commented / noted | `order:comment`, `order:note` | 12, 3.5 |
| comment count | `commentcount:>3` | 28 |
| suggested by the tagger, alone / with a common tag | `ai:long_red` | 32 / 49 |
| a user's 16 saved searches | `search:all` | 82 |

What keeps it fast:

- **Tag searches pick their strategy from exact tag counts.** Common tags
  walk the newest posts until a page is full; rare ones are collected
  through the tag index. PostgreSQL alone often guesses wrong for tag
  combinations.
- **Counts stop early.** Counts are exact up to `search.count_limit`
  (10,000). Counts that would still read too much, per PostgreSQL's
  estimate (`search.count_cost_limit`), are shown as estimates instead.
- **"Next" links use cursors**, which cost the same however deep they go;
  numbered pages stop at `search.max_page`.
- **Saved searches run four at a time** for `search:`, each contributing
  its newest 500 posts.

## Measuring your own

Seed a *separate* database, then benchmark it:

```sh
MOEKURA_DATABASE__URL=postgres://…/moekura_bench moekura admin seed --posts 5000000
MOEKURA_DATABASE__URL=postgres://…/moekura_bench moekura admin bench
```

Seeding generates everything in PostgreSQL, about 2,000 posts a second
(40 minutes for 5,000,000): posts with their comments and votes, notes,
pools, favorite groups, saved searches and tagger suggestions. `admin bench --explain NAME` prints the query plans of the matching
searches, and `--check` fails if a search that should be selective reads
more than half as many pages as the posts table has (CI runs that check on
200,000 posts).

## Tuning PostgreSQL

The defaults of a stock PostgreSQL are sized for a small machine. For a
large site, start from:

```text
shared_buffers = 25% of memory
effective_cache_size = 50–75% of memory
work_mem = 32MB
maintenance_work_mem = 1GB
random_page_cost = 1.1        # on SSDs
```
