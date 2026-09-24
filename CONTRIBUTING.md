# Contributing to Moekura

Thanks for helping! Bug reports, ideas, docs fixes and code are all
welcome.

## Before you start

- **Bugs and ideas:** open an [issue](https://github.com/moe-studios/moekura/issues/new/choose).
  For anything security-related, follow [SECURITY.md](SECURITY.md) instead.
- **Bigger changes:** open an issue first so we can agree on the approach.
  The plan and roadmap are in [docs/design.md](docs/design.md), and work
  is tracked in [milestones](https://github.com/moe-studios/moekura/milestones)
  and on the [project board](https://github.com/orgs/moe-studios/projects/1).

## Setting up

[Development](docs/src/development.md) in the book covers the tools you
need, running the tests against PostgreSQL, and building the TypeScript.

## Making a pull request

- Keep each pull request to one change, and add tests for it.
- Before pushing, make sure these pass:

  ```sh
  cargo fmt --all
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```

  If you changed `frontend/src`, also run `npm run check`, `npm test` and
  `npm run build` in `frontend/` and commit the rebuilt bundle.
- Write commit messages as [conventional commits](https://www.conventionalcommits.org/),
  such as `feat(search): add a width filter` or `fix(web): keep the page
  after logging in`. The changelog is generated from them.
- Update the book in `docs/src/` when you change behaviour, configuration
  or the API.

By contributing, you agree that your work is licensed under the
[AGPL-3.0-only](LICENSE), like the rest of the project, and to follow the
[code of conduct](CODE_OF_CONDUCT.md).
