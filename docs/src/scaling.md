# Scaling

A single `uwubooru serve` with PostgreSQL on the same machine is enough for
a private collection or a small community. As a site grows, the same
program scales out:

- run web servers behind a load balancer; they keep no state of their own;
- move background jobs to [separate `uwubooru worker` processes](admin/jobs.md);
- add PostgreSQL read replicas (`database.replicas`), which take searches
  and listings;
- store files in [S3-compatible storage](admin/storage.md) with a CDN in
  front;
- with several web servers, set `database.auto_migrate = false` and run
  `uwubooru migrate` when deploying.

## How fast is search?

Measured with 5,000,000 synthetic posts (240,000 tags, 6.9 GB database) on
a 16-core machine with 32 GB of memory, PostgreSQL 18 given
`shared_buffers = 4GB`. Each search is what a results page runs: looking up
the tags, fetching the page, and counting. Times are the 95th percentile of
20 runs, in milliseconds:

| Search | Example | ms |
|---|---|---|
| front page | | 0.3 |
| a tag on 70% of posts | `red_red` | 0.6 |
| two / three common tags | `red_red blue_red` | 5 / 8 |
| a common tag without another | `blue_red -red_red` | 17 |
| a rare tag (200 posts) | `eyes_fish_3` | 1.7 |
| a common and a rare tag | `red_red eyes_fish_3` | 9.5 |
| either of two tags | `~old_red ~new_red` | 31 |
| wildcards | `old_*`, `*_red` | 27, 6 |
| file type and a tag | `filetype:mp4 blue_red` | 48 |
| rating and score | `rating:e score:>20` | 1.5 |
| a year, with a tag | `date:2021`, `blue_red date:2021` | 2, 5.5 |
| page 500 of a common tag | | 6 |
| a cursor deep into a common tag | `page=b2500000` | 0.7 |

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

## Measuring your own

Seed a *separate* database, then benchmark it:

```sh
UWU_DATABASE__URL=postgres://…/uwu_bench uwubooru admin seed --posts 5000000
UWU_DATABASE__URL=postgres://…/uwu_bench uwubooru admin bench
```

Seeding generates everything in PostgreSQL, about 7,000–10,000 posts a
second. `admin bench --explain NAME` prints the query plans of the matching
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
