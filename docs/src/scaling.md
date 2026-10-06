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
given `shared_buffers = 4GB`, with release 0.5.0. Each search is what a
results page runs:
looking up the tags, fetching the page, and counting. Times are the 95th
percentile of 20 runs, in milliseconds:

| Search | Example | ms |
|---|---|---|
| front page | | 0.2 |
| a tag on 70% of posts | `red_red` | 0.5 |
| two / three common tags | `red_red blue_red` | 7.5 / 16 |
| a common tag without another | `blue_red -red_red` | 20 |
| a rare tag (200 posts) | `detailed_back_3` | 1.1 |
| a common and a rare tag | `red_red detailed_back_3` | 8.9 |
| either of two tags | `~cold_red ~old_red` | 38 |
| wildcards | `cold_*`, `*_red` | 42, 6.3 |
| file type and a tag | `filetype:mp4 blue_red` | 1.4 |
| rating and score | `rating:e score:>20` | 1.1 |
| a year, with a tag | `date:2022`, `blue_red date:2022` | 1.7, 5.2 |
| page 500 of a common tag | | 6.3 |
| a cursor deep into a common tag | `page=b2500000` | 0.5 |
| a pool, in its own order | `pool:23`, `ordpool:23` | 1.4, 1.3 |
| posts in any pool | `pool:any` | 46 |
| a favorite group (300 posts) | `favgroup:1235` | 1.8 |
| a word in notes, common / rare | `note:red`, `note:new` | 77 / 26 |
| recently commented / noted | `order:comment`, `order:note` | 2.5, 3 |
| comment count | `commentcount:>3` | 24 |
| suggested by the tagger, alone / with a common tag | `ai:long_red` | 27 / 38 |
| a user's 16 saved searches | `search:all` | 103 |

What keeps it fast:

- **Tag searches pick their strategy from exact tag counts.** Common tags
  walk the newest posts until a page is full; rare ones are collected
  through the tag index. PostgreSQL alone often guesses wrong for tag
  combinations.
- **Counts stop early.** Counts are exact up to `search.count_limit`
  (10,000). Counts that would still read too much, per PostgreSQL's
  estimate (`search.count_cost_limit`), are shown as estimates instead.
- **"Next" links use cursors**, which cost the same however deep they go;
  numbered pages stop at the deepest page allowed
  ([Search pages](configuration.md#search-pages)).
- **Saved searches run four at a time** for `search:`, each contributing
  its newest 500 posts.

## Measuring your own

Seed a *separate* database, then benchmark it:

```sh
MOEKURA_DATABASE__URL=postgres://…/moekura_bench moekura admin seed --posts 5000000
MOEKURA_DATABASE__URL=postgres://…/moekura_bench moekura admin bench
```

Seeding generates everything in PostgreSQL, about 1,350 posts a second
(an hour for 5,000,000): posts with their comments and votes, notes,
pools, favorite groups, saved searches and tagger suggestions. `admin bench --explain NAME` prints the query plans of the matching
searches, and `--check` fails if a search that should be selective reads
more than half as many pages as the posts table has (CI runs that check on
200,000 posts).

## How fast are pages?

Searches are only part of a page: it also loads the posts, their tags,
comments and notes, and renders. `moekura admin bench-http` times whole
responses from a running server, picking what to load from its database:
the busiest posts (with comments, notes and a pool), common and rare tags,
the biggest pools. Point it at a server using the seeded database:

```sh
export MOEKURA_SERVER__API_REQUESTS_PER_MINUTE=0   # no API rate limit
MOEKURA_DATABASE__URL=postgres://…/moekura_bench moekura serve &
MOEKURA_DATABASE__URL=postgres://…/moekura_bench moekura admin bench-http --url http://localhost:8080
```

Requests are anonymous, one at a time (`--concurrency` for more); a
target with several posts or tags loads them in turn. `--check-ms 100`
fails if a p95 is above 100 ms, and any response other than 200 OK fails
the run.

On the same 5,000,000 posts and machine, with the server (0.5.0's
image) and the benchmark on it too, p95 in milliseconds, for one request
at a time and for 16:

| Page | Path | 1 | 16 |
|---|---|---|---|
| front page | `/` | 3.2 | 7.9 |
| a common tag | `/posts?tags=red_red` | 4.2 | 8.6 |
| two common tags | `/posts?tags=red_red+blue_red` | 3.9 | 8.7 |
| rare tags | `/posts?tags=detailed_back_3` | 4.6 | 11 |
| posts with 20–33 comments, notes and a pool | `/posts/67561` | 3.7 | 7 |
| pools | `/pools/23` | 2.9 | 5.9 |
| newest comments | `/comments` | 5 | 9 |
| tag list | `/tags` | 0.7 | 2.8 |
| API search, with a tag | `/api/v1/posts?tags=red_red` | 3.3 | 10 |
| API post | `/api/v1/posts/67561` | 0.8 | 2.5 |
| Danbooru search, with a tag | `/posts.json?tags=red_red` | 3 | 9.9 |
| Danbooru post | `/posts/67561.json` | 2.2 | 4.8 |
| autocomplete (site, API, Danbooru) | `/tags/autocomplete?q=re` | 1.8 | 3.1 |
| feed, with a tag | `/posts.atom?tags=red_red` | 3.6 | 8.5 |

Common tags come out faster than in the search table because counts
that reach the count limit are reused for `cache.count_ttl_secs` (30
seconds); one visitor in that time pays for the count.

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
