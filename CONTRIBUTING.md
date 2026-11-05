# Contribute to RustChan

You can help by reporting a bug, improving the docs, or making a focused code
change. You do not need to build RustChan to suggest a documentation correction.
Start by checking existing [issues](https://github.com/csd113/RustChan/issues)
and pull requests for related work.

[All guides](README.md#documentation) · [Support](SUPPORT.md) · [Code of Conduct](CODE_OF_CONDUCT.md)

For a security defect, read [SECURITY.md](SECURITY.md) before posting details.
Keep credentials, private media, personal information, and real instance data out
of reports, commits, screenshots, and fixtures. Discuss the problem and scope
before starting a large behavior or architecture change.

## Build and run

Install Git, [Rust through rustup](https://rustup.rs), and your platform's native
build tools. The supported minimum Rust version is **1.99.1**, also used in CI.
Linux needs a C/C++ build toolchain; macOS needs Xcode Command Line Tools; Windows
MSVC builds need the Visual Studio C++ build tools and Windows SDK.

```sh
git clone https://github.com/csd113/RustChan.git
cd RustChan
cargo build --locked --release
```

On macOS/Linux, start a local trial with:

```sh
CHAN_HOST=127.0.0.1 CHAN_TOR_SUPPORT=false ./target/release/rustchan-cli
```

On Windows, use PowerShell:

```powershell
$env:CHAN_HOST = "127.0.0.1"
$env:CHAN_TOR_SUPPORT = "false"
.\target\release\rustchan-cli.exe
```

Follow [first setup](SETUP.md#finish-setup). Data defaults to
`target/release/rustchan-data`; use an absolute `--data-dir` if you want to keep it
outside build output. Do not use production data for development checks.
FFmpeg is [optional for media processing](SETUP.md#install-ffmpeg).

Default features include `tls-self-signed`. Other supported build modes are
`--no-default-features`, optional `--features tls-acme`, and `--all-features`.
Keep `rust-version` and `cargo-msrv` metadata aligned. Do not change the toolchain,
features, lockfile, or dependencies as unrelated cleanup; consider existing
crates and the standard library before adding a dependency.

## Make a focused change

Start from the current default branch, or the working branch agreed for your task:

```sh
git switch main
git pull --ff-only
git switch -c docs/short-description
```

Use a descriptive name such as `fix/`, `feature/`, or `docs/`. Keep unrelated work
intact. Preserve routes, element IDs, form fields, JavaScript/CSS hooks,
no-JavaScript fallbacks, and public APIs unless the change needs to update them.

A pull request should explain the problem and resulting behavior, list affected
areas and validation results, and identify migrations, compatibility concerns,
operator steps, or remaining risks. Link the issue when one exists. Do not put
uncoordinated vulnerability details in a public PR.

## Validation

For application changes, run the baseline checks:

```sh
cargo fmt --all --check
cargo check --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo
cargo build --locked --workspace --all-features
cargo test --locked --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
```

The last line uses POSIX-shell environment syntax. In PowerShell set
`$env:RUSTDOCFLAGS = "-D warnings"` before the `cargo doc` command.

The focused regressions use small synthetic inputs for CSRF/uploads, update
checks, retained recovery, log counting, console behavior, and backup safety.
For startup or request changes, add an appropriate disposable local smoke check.
Keep generated evidence out of commits.

Documentation-only changes can use documentation, YAML, and link checks instead
of compiling. There is no separate Markdown site build in this repository.
Check relative paths and heading links, keep README artwork references working,
validate examples against code, and always run:

```sh
git diff --check
python3 tools/check_local_tooling.py
```

### Binary-upgrade validation

The release workflow and separately dispatched
[upgrade workflow](.github/workflows/upgrade.yml) run
[`src/db/tests/binary_upgrade.py`](src/db/tests/binary_upgrade.py) against
disposable 1.7.1 installations. This slower gate is separate from normal push CI
and Cargo tests. Follow that workflow's inputs; do not use a live installation.

The Windows 1.7.1 baseline has a drive-prefix bug that returns 404 for board
media. The report records that limitation and checks original bytes on disk,
including after rollback. The candidate must serve exact retained bytes with
HTTP 200 after upgrade and restart; it never inherits the baseline exception.

## Code and data changes

The workspace lint tables in [Cargo.toml](Cargo.toml) define Rust/Clippy policy;
unsafe Rust is forbidden. New workspace packages inherit it with
`[lints] workspace = true`. Keep lint exceptions narrow with a reason at the
code. The patched AAC crate in `vendor/` is outside the workspace and retains
its upstream lint configuration. Review dependency exceptions in [deny.toml](deny.toml).

Treat posts, uploads, archives, paths, cookies, headers, configuration, environment
variables, and databases as untrusted. Validate them before changing files,
writing data, redirecting, restoring, or launching a process. Authentication,
moderation, backup, restore, and migration work should stop safely when state is
uncertain and avoid partial-success outcomes.

Released schema changes need a forward migration tied to a release. Preserve data
and reject unknown, partial, or corrupt schemas. Validate relevant fresh, upgrade,
invalid-input, rollback, and backup/restore paths under the test policy below.
See the [database guide](docs/sqlite-engineering.md).

### Tests and fixtures

Add a new test only when needed for an important concrete behavior or confirmed
regression. Explain the failure it catches, why existing validation is insufficient,
and its maintenance and execution cost first. Prefer the smallest focused test;
avoid speculative coverage, duplicate assertions, implementation-detail tests,
and expensive full-app tests by default. New tests belong in a clearly named
`tests` directory under their owning subsystem, with correct discovery wiring.
Do not run removed suites or recreate them solely to replace deleted coverage.

Fixtures must be small, synthetic or safe to redistribute, and free of private
instance data or identifying metadata.

## Documentation and generated files

When behavior changes, update its authoritative guide and add a concise changelog
entry. Keep new work under **Unreleased** until it ships; move it into the named
release section when preparing that release. Preserve published release history.
Each permanent document needs a continuing purpose. Prefer linking to one guide
over repeating the same instructions across files.

Keep completed audits, AI activity reports, implementation plans, debugging
transcripts, benchmarks, and temporary validation output out of Git. Put useful
verified conclusions in the relevant guide. Use Git history for past investigations;
do not create obsolete report archives. Preserve operational, security, licensing,
compliance, and release records.

Use ignored `output/` or `audit-output/`, or a location outside the checkout, for
disposable evidence. Keep test infrastructure separate from its output.
`tools/check_local_tooling.py` verifies that designated local instructions and
generated evidence remain untracked.

Do not hand-edit generated output when a source or generator is authoritative.
Commit generated files only when intentionally versioned and required; explain
how they were regenerated. Never commit runtime data, databases, backups, logs,
credentials, TLS/Tor keys, private media, or large temporary artifacts. Do not
promise anonymity, perfect security, availability, or legal compliance.

## License

Contributions are provided under RustChan's [MIT license](LICENSE). Preserve
third-party licenses and [vendored patch provenance](vendor/symphonia-codec-aac/RUSTCHAN-PATCH.md).
