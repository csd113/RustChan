# RustChan AAC patch

This small patch prevents incomplete HE-AAC audio from being treated as a
successful decode. It detects unsupported spectral band replication (SBR), so
RustChan can request its FFmpeg compatibility path instead.

## Upstream source and license

- Source: crates.io `symphonia-codec-aac` **0.6.1**, from
  [Symphonia](https://github.com/pdeljanov/Symphonia).
- License: **MPL-2.0**; the original source, manifest, tests, and license are preserved.
- Upstream archive SHA-256:
  `f5bf8e39552d34a3c4c98333370e62f48c92456d2e814f273b5c3ad7c4a5f45c`.

No additional dependencies, native code, or build script are introduced.

## Changes

The only source change is in `src/aac/mod.rs`: after a complete raw-data block is
parsed, return `Unsupported("aac: spectral band replication is unsupported")`
when the existing parser detected an in-band SBR fill element. Without this,
upstream synthesizes only AAC-LC core samples and silently changes HE-AAC rate,
duration, and bandwidth. RustChan recognizes that exact error as a request for
compatibility decoding. Truncated payloads keep their existing parse errors.

The manifest's `lazy_static` lower bound was updated to 1.5.1 on 2026-10-03;
the decoder patch did not change. Upstream requires Rust 1.85; RustChan's
workspace requires 1.99.1.

## When to remove the patch

This detects a missing capability; it does not implement SBR or parametric
stereo. Remove it when an upstream release provides correct reconstruction or
the same explicit error. Preserve this provenance and the upstream license while
the patch is in use.
