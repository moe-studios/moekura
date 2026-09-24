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
See [Upgrading](docs/src/upgrading.md) for how to move to it.

## Scope

Anything that lets someone read, change or delete what they shouldn't,
run code on the server, or get around a site's private mode, bans or rate
limits is in scope. So are problems in the published container image.

Problems that need an admin account, or a configuration the docs warn
against, usually aren't vulnerabilities, but tell us anyway if you're
unsure.
