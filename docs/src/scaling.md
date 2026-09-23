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

Measurements and tuning advice for sites with millions of posts will be
added here.
