# Contributing to RustChan

RustChan is self-hosted imageboard software written in Rust. Contributions are
welcome when they are focused, testable, and safe for operators and people who
use independently run instances.

By participating, you agree to the [Code of Conduct](CODE_OF_CONDUCT.md). For a
security vulnerability, follow [SECURITY.md](SECURITY.md) before sharing any
details publicly. General support boundaries are in [SUPPORT.md](SUPPORT.md).

## Before starting

- Search existing issues and pull requests for related work.
- Use the bug, feature, or documentation issue form when an issue is useful and
  issue creation is available to your account.
- Keep secrets, personal information, private media, and real instance data out
  of all issues, commits, fixtures, logs, and screenshots.
- For a large behavioral or architectural change, establish the problem and
  intended scope before investing in an implementation.

The project does not operate or control independent RustChan instances. Do not
submit another site's content, moderation dispute, abuse report, database, or
operational secrets as repository evidence.

## Branches and pull requests

Create a focused branch from the current default branch:

```sh
git switch main
git pull --ff-only
git switch -c fix/short-description
```

Use a descriptive prefix such as `fix/`, `feature/`, or `docs/`. Keep commits
and the pull request narrowly scoped. Preserve unrelated behavior, route names,
element IDs, form fields, JavaScript and CSS hooks, no-JavaScript fallbacks, and
public APIs unless the change specifically requires otherwise.

In the pull request:

- explain the problem and the chosen approach;
- identify behavior and deployment surfaces that changed;
- list the exact validation commands run and their results;
- call out migrations, compatibility concerns, operational steps, or remaining
  risks; and
- link the relevant issue when one exists.

Do not include vulnerability details in a public pull request unless disclosure
has already been coordinated under [SECURITY.md](SECURITY.md).

## Rust and feature policy

The workspace currently declares Rust `1.91` as its minimum supported Rust
version (MSRV). Keep `rust-version` and the `cargo-msrv` metadata aligned; do not
raise the MSRV incidentally.

The current Cargo feature sets are:

- default features, which include `tls-self-signed`;
- `--no-default-features`; and
- optional `tls-acme` or `--all-features` builds.

Do not change toolchain, feature, lockfile, or CI policy as unrelated cleanup.
Do not add a dependency until the standard library and existing crates have
been considered.

## Validation

Start with the narrowest test that covers the change. The repository's baseline
Rust checks are:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-features
```

Use a local Playwright harness for public UI, admin UI, media, backup and restore,
moderation, Tor/proxy, and no-JavaScript changes. Browser-test infrastructure is
local-only: keep package manifests, configs, scripts, fixtures, and artifacts
under the matching `.gitignore` rules. Do not add them to commits or CI workflows.
Run focused scenarios first and the available browser matrix for broad changes.
Some media tests require `ffmpeg`, `ffprobe`, and specific codecs; do not replace
deterministic fixtures with private uploads from a live site. Permanent Rust
unit and integration tests remain normal repository source.

Run the full Rust validation sequence before handoff for application changes.
Documentation-only changes may use the repository's applicable documentation,
YAML, and link checks instead of compiling the application. Always run:

```sh
git diff --check
```

## Change-specific expectations

### Database and migrations

Released schema changes need a forward migration tied to a RustChan release.
Preserve data and fail closed on partial, unknown, or corrupt schemas. Add tests
for fresh creation, supported upgrade paths, invalid input, rollback, and
backup/restore interaction as applicable. Do not silently rewrite or discard an
operator's database.

### Tests and fixtures

Keep tests close to the behavior they protect. Media fixtures must be small,
synthetic or redistribution-safe, and stripped of metadata and identifying
content. Include JavaScript-disabled coverage when changing an existing no-JS
flow. Security-sensitive changes need negative tests and failure-path coverage.

### Documentation and changelog

Update `README.md`, `SETUP.md`, generated configuration guidance, or other
operator documentation when behavior, defaults, features, storage, Tor, TLS,
backups, moderation, or ChanNet changes. Add a concise entry under the matching
subsection in the top `CHANGELOG.md` section for notable changes. Do not promise
anonymity, perfect security, availability, legal compliance, or protection from
a malicious operator.

### Generated files

Do not hand-edit generated output when a source or generator is authoritative.
Commit generated files only when they are intentionally versioned and required
by the change, and document how they were regenerated. Avoid large generated
artifacts and temporary validation evidence.

### Sensitive paths

Treat public posts, uploads, archive contents, paths, cookies, headers,
environment variables, configuration, databases, backups, and ChanNet payloads
as untrusted. Validate them before filesystem mutation, database writes,
redirects, restore operations, or process execution. Backup, restore,
migration, authentication, administration, and moderation changes should avoid
partial-success states and fail closed where safety is uncertain.

Never commit runtime data, `rustchan-data/`, SQLite databases, backups, logs,
credentials, tokens, TLS private keys, Tor/onion private keys, private media,
temporary evidence, Playwright reports, or other large generated artifacts.

## License

RustChan is licensed under the [MIT License](LICENSE). By contributing, you
agree that your contribution is provided under that license.
