# Security policy

## Reporting a vulnerability

Please don't open a public issue. Report it privately through
[GitHub's private vulnerability reporting](https://github.com/moe-studios/moekura/security/advisories/new)
instead, with the version (or commit), how to reproduce it and what an
attacker could do with it.

You should hear back within a week. Once a fix is released, the advisory
is published with credit to you, unless you'd rather not be named.

## Supported versions

Moekura is before 1.0, so only the latest release gets security fixes.
See [Upgrading](docs/src/upgrading.md) for how to move to it, and
[Stability](docs/src/stability.md) for what upgrading within a major
version keeps working.

From 1.0 on:

| Release | Security fixes |
|---|---|
| The latest minor release (1.N) | yes, as patch releases (1.N.1, 1.N.2, …) |
| The minor release before it (1.N−1) | for 3 months after 1.N comes out |
| The last minor release of the previous major (1.x once 2.0 is out) | for 6 months after 2.0 comes out |
| Anything older, and unreleased builds (`edge`, `git-…`) | no |

A fix for an older release comes as a patch release of that release, so
a site can take it without upgrading to a new minor release first. Fixes
are released before the advisory is published.

## Scope

Anything that lets someone read, change or delete what they shouldn't,
run code on the server, or get around a site's private mode, bans or rate
limits is in scope. So are problems in the published container image.

Problems that need an admin account, or a configuration the docs warn
against, usually aren't vulnerabilities, but tell us anyway if you're
unsure.
