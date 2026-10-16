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

The workspace currently declares Rust `1.99.1` as its minimum supported Rust
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

The repository's baseline Rust checks are:

```sh
cargo fmt --all --check
cargo check --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo
cargo build --locked --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
```

Use a small runtime smoke check with disposable local data when application
startup or request handling changes. Keep generated evidence out of commits.

Run the full Rust validation sequence before handoff for application changes.
Documentation-only changes may use the repository's applicable documentation,
YAML, and link checks instead of compiling the application. Always run:

```sh
git diff --check
```

## Change-specific expectations

### Database and migrations

Released schema changes need a forward migration tied to a RustChan release.
Preserve data and fail closed on partial, unknown, or corrupt schemas. Validate
fresh creation, supported upgrade paths, invalid input, rollback, and backup/restore
interaction as applicable, subject to the essential-value test policy below.
Do not silently rewrite or discard an operator's database.

### Tests and fixtures

New tests are permitted only when absolutely needed to protect a concrete,
important behavior or a confirmed regression. Before adding one, explain the
failure it catches, why existing validation is insufficient, and its maintenance
and execution cost. Prefer the smallest focused test. Do not add speculative
coverage, duplicate assertions, trivial implementation-detail tests, or expensive
full-application tests by default. Any future tests must live in a clearly named
`tests` directory under their owning subsystem, with correct discovery wiring.

Any essential fixtures must be small, synthetic or redistribution-safe, and
free of private instance data and identifying metadata.

### Documentation and changelog

Update `README.md`, `SETUP.md`, generated configuration guidance, or other
operator documentation when behavior, defaults, features, storage, Tor, TLS,
backups, or moderation changes. Add a concise entry under the matching
subsection in the top `CHANGELOG.md` section for notable changes. Do not promise
anonymity, perfect security, availability, legal compliance, or protection from
a malicious operator.

Permanent documentation should describe the current software, supported
workflows, or necessary design decisions. Each new document needs a continuing
maintenance purpose. Update the authoritative guide when implementation changes;
avoid duplicating the same system across files and prefer concise explanations.

Keep AI activity summaries, completed audits and implementation plans, debugging
transcripts, benchmark output, and temporary validation evidence out of version
control. Incorporate useful, verified conclusions into the existing guide, then
remove the completed report or plan. Use git history for past investigations;
do not retain obsolete narratives in archives or giant consolidated reports.
One-off work generally does not need a new Markdown file. Preserve required
operational, security, licensing, compliance, and release records.

Place disposable evidence under the ignored `output/` or `audit-output/`
directories, or outside the checkout. Keep permanent test infrastructure separate
from its generated results. Review links when moving or deleting documentation.
`tools/check_local_tooling.py` checks that local instructions, reserved browser
tooling paths, and designated generated evidence remain untracked.

The workspace lint tables in `Cargo.toml` are the authoritative Rust/Clippy
policy; unsafe Rust is forbidden. New workspace packages must inherit it with
`[lints] workspace = true`. The patched upstream AAC crate under `vendor/` is
excluded from the workspace and keeps its upstream lint configuration. Keep
lint expectations narrow, with an explicit reason at the affected code; review
dependency exceptions in `deny.toml` rather than copying generated inventories
into documentation.

### Generated files

Do not hand-edit generated output when a source or generator is authoritative.
Commit generated files only when they are intentionally versioned and required
by the change, and document how they were regenerated. Avoid large generated
artifacts and temporary validation evidence.

### Sensitive paths

Treat public posts, uploads, archive contents, paths, cookies, headers,
environment variables, configuration, and databases as untrusted. Validate them
before filesystem mutation, database writes, redirects, restore operations, or
process execution. Backup, restore, migration, authentication, administration,
and moderation changes should avoid partial-success states and fail closed where
safety is uncertain.

Never commit runtime data, `rustchan-data/`, SQLite databases, backups, logs,
credentials, tokens, TLS private keys, Tor/onion private keys, private media,
temporary evidence, Playwright reports, or other large generated artifacts.

## License

RustChan is licensed under the [MIT License](LICENSE). By contributing, you
agree that your contribution is provided under that license.
