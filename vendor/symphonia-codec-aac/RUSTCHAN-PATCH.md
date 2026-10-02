# RustChan AAC patch

Source: the crates.io `symphonia-codec-aac` 0.6.1 release, from [Symphonia](https://github.com/pdeljanov/Symphonia), licensed MPL-2.0. The upstream package archive checksum is `f5bf8e39552d34a3c4c98333370e62f48c92456d2e814f273b5c3ad7c4a5f45c`. Original source, manifest, tests and license are preserved. No additional dependencies, native code or build script are introduced.

The only source change is in `src/aac/mod.rs`: after parsing a complete raw-data block, return `Unsupported("aac: spectral band replication is unsupported")` when the existing parser has detected an in-band SBR fill element. Upstream otherwise synthesizes only AAC-LC core samples, silently changing HE-AAC rate, duration and bandwidth. RustChan recognizes this exact error as an explicit compatibility requirement. Truncated payloads keep their existing parse errors.

This patch detects a missing decoder capability; it does not implement SBR or parametric stereo. Remove the patch after an upstream release provides correct reconstruction or this same explicit error. The upstream crate requires Rust 1.85; RustChan still requires 1.99.
