# Thumbnail pipeline audit — 2026-10-04

Audited from `c7e4be7` on branch `1.7.0-staging`. No dependencies, schema,
thumbnail encoding, templates, CSS, post URLs, or production JavaScript changed.

**Lifecycle and existing behavior**

- Multipart uploads stream to temporary files. MIME/structure, board policy,
  size and decoded-image limits are validated before persistence. The existing
  `MediaUploadGate` permits one public media upload at a time, including parsing
  and processing. Decoder validation remains mandatory even for deduplicated
  uploads. Originals are capped at 40 million decoded pixels and decoder
  allocations at 256 MiB.
- `handlers::process_primary_upload` hashes the upload and consults `file_hashes`.
  A same-board hit reuses the stored original and thumbnail when both exist;
  missing outputs cause upload-time processing again. Empty thumbnail paths are
  valid for generic files. No content decode is performed on thumbnail GETs.
- `utils/files/storage.rs` stages UUID-named files, invokes
  `MediaProcessor::process_upload`, and records the actual thumbnail output path.
  Image conversion happens first; thumbnail generation decodes the persisted
  result. JPEG orientation and metadata stripping retain their existing behavior.
  Validation, conversion and thumbnailing can therefore decode a fresh image
  separately; that work happens on upload, never on page or thumbnail delivery.
- `media/thumbnail.rs` fits still previews into the configured square (default
  250 × 250), preserving aspect ratio and avoiding enlargement of small images.
  Triangle filtering and the `image` crate's lossless WebP encoding remain
  unchanged; there is no lossy quality setting. Animated media uses a still first
  composited frame for its preview; the original conversion preserves animation.
  PDF first pages use Hayro; video frames use optional FFmpeg then the same Rust
  WebP encoder. Failures can produce generated 250 × 250 SVG placeholders. Audio
  initially uses SVG, then durable workers publish waveform PNGs. Background
  workers are capped at four; queued job claiming/completion is transactional.
- Upload finalization persists post/file-hash paths together with filesystem
  recovery intent. Referenced thumbnails survive cache eviction. Ordinary
  page loads do not regenerate previews. Generation failure can leave an original
  without a thumbnail; pending/failed/pruned media has dedicated template states.
- Board, catalog, archive, thread and preview/update APIs render stored paths
  into `/boards/{board}/thumbs/{filename}` URLs. Rendering does not stat or decode
  thumbnail files. Catalog/index/archive images are lazy with async decoding;
  thread OPs are eager and replies lazy. CSS limits post previews to 150 px and
  replies to 80 px (with responsive overrides). Posts store no thumbnail
  width/height metadata, so post images have no intrinsic-size HTML hints.
- `static/main.js` installs one load/error fallback per image; an already complete
  image with no natural size may call `decode()` before showing the fallback.
  There is no custom thumbnail download queue or original-image prefetch. The
  full image's `src` is assigned from `data-src` on expansion; videos/audio use
  `preload="none"`, PDFs use `about:blank` until opened. Native browser request
  scheduling and caching handle concurrency and repeated URLs. Cross-board hover
  previews also deduplicate in-flight HTML fetches and retain a per-page HTML
  cache; pending media is refreshed through the existing post-update endpoint. No-JS links still
  open the original. Sensitive-content/NSFW, quote-preview and media hooks remain
  intact.
- `/boards/{*media_path}` checks live board permissions before every origin
  request, including conditional GETs. Path validation canonicalizes root and
  target, checks every path component for symlinks, verifies a regular file and
  rejects multiple hardlinks on Unix. The leaf is inspected during the component
  walk and again after canonicalization; the file service then obtains metadata
  from its opened file. Those checks were preserved. `ServeFile` streams only the
  requested file in bounded chunks (default 64 KiB). It supports HEAD, byte
  ranges, Last-Modified/If-Modified-Since and metadata-derived
  ETag/If-None-Match; ETags do not hash/read image contents. GET/304 still opens
  the file and reads metadata; 304 does not read a payload. WebP/PNG are not
  HTTP-compressed again. Generated SVGs retain restrictive CSP/nosniff headers.
- Public UUID media already had `public, max-age=31536000, immutable`.
  View-password media uses `private, no-cache, must-revalidate`; unversioned
  replaceable board favicons use a one-hour TTL. Revalidated private media still
  checks access. Missing SVG waveforms redirect temporarily to their completed
  PNGs; old MP4/MKV URLs redirect permanently to WebM siblings. Corrupt persisted
  thumbnail bytes receive an ordinary file response and the browser displays a
  fallback; serving never decodes or repairs them.

**Findings and implemented changes**

The browser baseline did not expose duplicate downloads, eager original loading,
far-offscreen unique thumbnail floods, or unnecessary warm-cache media requests.
The worthwhile changes were on the server request path:

1. Synchronous canonicalization, per-component filesystem checks and legacy
   sibling resolution previously ran on the async executor after DB preflight.
   They now run in that same blocking preflight. The DB connection is released
   before filesystem work. A rare replacement/open race also resolves its legacy
   sibling on a blocking worker. Security checks and their syscall count remain
   intact; the improvement is executor responsiveness and overlapping work.
2. A public/post-password request carrying an admin cookie previously performed a
   session SELECT despite already having view access. Session lookup now occurs
   only when an unresolved view-password gate requires administrator access.
   Valid board unlock cookies also avoid that extra lookup. Board access is still
   loaded from the DB each time; no authorization cache was introduced. Public
   cookie-bearing requests go from two application SELECTs to one (code-path
   accounting; not a SQL trace). Pool validation SQL is separate.
3. Media errors now carry private no-store headers. Previously an initial 404 had
   no explicit cache policy, and a failed open after successful validation could
   inherit the one-year immutable policy. A missing/generated-later file or a
   temporary denial must not be retained as immutable content. This is a
   correctness improvement established from the response path; no negative-cache
   performance win is claimed. Successful 200/206/304 policies remain unchanged.
4. Response metadata uses the validated file path, avoiding the earlier redundant
   root/path construction. No large image buffers or application cache were added.

**Measured evidence**

The tracked ignored Rust workload runs the production handler and `ServeFile`
through real loopback HTTP/1.1: 128 pre-generated 250 px WebP files, concurrency
16, five batches for each visitor/admin-cookie and full/conditional combination.
The original is deleted before requests, proving delivery is independent of
source decoding/resizing. It measures complete-response latency, body bytes and
DB checkouts, with cumulative CPU/end-of-batch RSS observations from `ps`.

Apple M2 Pro, arm64 macOS, Rust 1.99.0, debug build, current-thread Tokio harness,
warm filesystem/server cache. These are local workload results, not a release
capacity claim. Moving blocking calls can have a different timing effect on a
multi-thread production runtime or slower storage.

| 128-request batch | Before median ms | After median ms | Before request p50/p95 ms | After request p50/p95 ms |
| --- | ---: | ---: | ---: | ---: |
| Visitor, full responses | 30.023 | 22.027 | 3.341 / 4.425 | 2.514 / 3.108 |
| Visitor, conditional 304 | 25.653 | 18.195 | 2.804 / 3.498 | 1.878 / 2.603 |
| Admin cookie, full responses | 31.436 | 22.624 | 3.621 / 4.376 | 2.589 / 3.475 |
| Admin cookie, conditional 304 | 27.288 | 18.924 | 2.897 / 4.983 | 1.983 / 2.664 |

An independent optimized repeat recorded 21.414, 17.841, 22.432 and 18.891 ms,
respectively. Every full batch transferred 71,168 body bytes before and after;
304 batches transferred zero body bytes. All origin batches checked out 128 DB
connections. No reduction in filesystem calls, total CPU or RSS is established.
`ps` includes client/setup/instrumentation and is not a peak-memory or isolated
server-CPU measurement. Syscalls and SQL statements were not traced on this host.

The local browser workload extends `tests/e2e/helpers.ts` and diagnostics rather
than introducing a framework. It uploads a real 1024 × 768 gradient, then seeds
0, 4 and 128 image-post cases. The many-post page has 65 unique thumbnails,
including repeated URLs. CDP observes cache/response events without request
routing (routing would disable browser cache); Resource Timing observes bytes.
Five visits per size include a cleared browser cache followed by four warm visits.

| Browser case | Before / after unique origin thumbnail responses | Before / after Resource Timing transfer bytes |
| --- | ---: | ---: |
| No images, cold or warm | 0 / 0 | 0 / 0 |
| Four images, cold | 4 / 4 | 133,264 / 133,264 |
| Four images, warm | 0 / 0 | 0 / 0 |
| 128 posts, initial cold viewport | 25 / 25 | 832,900 / 832,900 |
| 128 posts, warm viewport | 0 / 0 | 0 / 0 |
| 128 posts, complete scroll | 65 / 65 total unique URLs | No duplicate URL requests |

Browser WebP previews were 33,016 bytes in both runs. The initial many-post
viewport transferred 825,400 body bytes plus Resource Timing's estimated header
cost. Some far-offscreen repeated elements obtain natural dimensions from shared
cached URLs; that does not represent additional network downloads. Cold load-event
completion was 52.0 ms before and 52.8 ms after. Warm navigation timings varied,
including the no-image control, so they do not establish a frontend speedup.
Opening the image loaded the correct 1024 px original; missing/corrupt previews
showed fallbacks, and a missing file created later recovered on navigation.

Raw HTTP samples: [baseline](thumbnail-evidence/http-baseline.json),
[optimized](thumbnail-evidence/http-final.json),
[repeat](thumbnail-evidence/http-final-repeat.json). Browser aggregate/per-resource
observations: [baseline](thumbnail-evidence/browser-baseline.json),
[optimized](thumbnail-evidence/browser-final.json). Full browser artifacts remain
under the repository's ignored `output/playwright/thumbnails/` directory. The new
browser spec remains local under ignored `tests/e2e/`, following `.gitignore`'s
existing policy; the Rust benchmark and evidence are versionable.

**Validation and reproduction**

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo
cargo test --workspace --all-features
cargo test --workspace --all-features -- --test-threads=1
RUSTCHAN_THUMB_EVIDENCE=/tmp/thumbnail-http.json cargo test --locked --lib server::handlers::board::media::performance::thumbnail_http_workload -- --ignored --exact
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/thumbnails/check npx playwright test tests/e2e/thumbnail-loading.spec.ts --project=chromium --workers=1
RUSTCHAN_E2E_SKIP_BUILD=1 npx playwright test tests/e2e/media.spec.ts tests/e2e/password-boards.spec.ts tests/e2e/phase3-media-runtime.spec.ts tests/e2e/non-video-media.spec.ts --project=chromium --project=webkit --project=chromium-nojs --workers=2
```

Build the current server with `cargo build --locked --bin rustchan-cli` before
using `RUSTCHAN_E2E_SKIP_BUILD=1`. Browser setup also generates its native update
fixture using the existing global setup.

Formatting and strict Clippy passed. New request regressions cover ETag/date 304,
HEAD, byte ranges, original independence, missing-file recovery/no-store,
permission changes, unlock retirement, session revocation, and legacy redirects.
The browser matrix passed 29 tests with 10 existing engine/capability skips;
the many-image benchmark passed before and after. Full parallel Rust runs exposed
shared global configuration-save locking and thumbnail-failure-injection
interference with a PDF test. The complete serial run passed: 1,353 library
tests, 1,353 binary tests, five CLI runtime tests, one native UI fixture test and
one doc test (2,713 passes total; 10 intentional ignores across library/binary).
No unrelated test or production configuration logic was modified.

**Deliberately deferred**

- No stronger public caching, application thumbnail cache, static-route bypass,
  or permission cache: existing warm visits already generate zero media origin
  work, while private media must recheck authorization.
- No lossy codecs, smaller previews or extra responsive variants: preview quality,
  compatibility and existing high-DPI presentation take precedence; network bytes
  did not regress. Lossless gradient previews can be relatively large.
- No runtime dimension probing. Intrinsic dimension metadata would require a
  coordinated schema/upload/restore/template change; guessing square dimensions
  would alter portrait/landscape layout. Profile layout shifts before doing that.
- No shared decoded-image upload pipeline, new generation queue, or single-flight
  cache: upload processing is already gated, requests never generate thumbnails,
  and fusing validation/conversion would broaden this change into media semantics.
- No reduction of symlink/hardlink/canonicalization checks, permanent root trust
  cache, or alternate file-server implementation: the checks protect untrusted
  stored paths. A descriptor-based race-hardening design is separate work.
- Public immutable retention and cached corrupt-file repair at the same URL retain
  their existing semantics; replacing immutable assets should use a new URL.

**Files changed**

- `src/handlers/board/media.rs`: delivery preflight, error cache policy and request regressions.
- `src/handlers/board/media/performance.rs`: opt-in actual HTTP workload.
- `docs/thumbnail-pipeline-audit.md` and `docs/thumbnail-evidence/{http-baseline,http-final,http-final-repeat,browser-baseline,browser-final}.json`: audit and measured evidence.
- Local ignored `tests/e2e/thumbnail-loading.spec.ts`: browser workload and recovery assertions.
