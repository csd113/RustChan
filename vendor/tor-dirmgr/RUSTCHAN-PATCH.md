# Tor directory manager compatibility backport

This is the crates.io `tor-dirmgr` 0.48.0 package, identified by SHA-256
`86f7ac56b491b26a6f2b73f0f16fa7a0c66b507163e0f0ed850334b86169b326`.
The upstream standalone lockfile and pre-normalization manifest are omitted.
The upstream MIT and Apache-2.0 license texts come from the Arti 2.8.0 source tag.

The only dependency-source change is the Rusqlite upper bound in `Cargo.toml`,
matching the `tor-dirmgr` part of [Arti's upstream compatibility change].
Rust source and enabled Arti features are unchanged. This permits a single
fixed bundled SQLite linkage for RustChan and Arti. Remove the patch when a
published Arti family release supports the selected Rusqlite version.

[Arti's upstream compatibility change]: https://gitlab.torproject.org/tpo/core/arti/-/commit/00a62585bcfab121797b6f42393149b97b7c231e
