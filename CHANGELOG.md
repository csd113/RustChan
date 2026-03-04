# Changelog

All notable changes to RustChan will be documented in this file.

## [1.0.3] - 2026-03-03

### Changed
- **Binary renamed** from `rustchan` to `rustchan-cli` to avoid filesystem conflicts with the `RustChan/` source directory on case-insensitive filesystems (macOS).

### Added
- **Dynamic upload progress bar** — while a file upload is in progress, a live spinner and pulsing bar are shown in the terminal stats output (e.g. `⠹ UPLOAD  [██████░░░░]  2 file(s) uploading`).
- **Requests per second counter** — the stats line now includes a live req/s figure computed over the interval since the last tick (e.g. `4.5 req/s`).
- **Board-specific stats** — below the main stats line, per-board thread and post counts are shown (e.g. `/b/  threads:12 posts:89  │  /tech/  threads:5 posts:22`).
- **New-event highlighting** — when the stats tick detects newly created threads or posts since the last check, those counts are printed in bold yellow with a `(+N)` delta indicator.
- **Active connections / users online** — the stats output now shows the count of unique client IPs active within the last 5 minutes and lists up to 5 of them (e.g. `users online: 3  │  IPs: 192.168.1.2, 192.168.1.5`).
- **Keyboard-driven admin console** — an interactive prompt is available while the server is running. Commands: `[s]` show stats, `[l]` list boards, `[c]` create board, `[d]` delete thread, `[u]` clear thumbnail cache, `[h]` help, `[q]` quit hint.

---

## [1.0.2] - 2026-03-03

### Changed
- **Frutiger Aero**: Softened the background gradient from saturated electric sky-blue to a cooler, more muted pearl-slate. Border glow pulled back from `#38b6ff` to a dusty steel blue (`#6aaed6`). Glass panels now feel frosted rather than bright. Button styles added to match the new palette.
- **NeonCubicle**: Replaced blinding pure cyan (`#0FF0FF`) borders and hot magenta (`#FF00AA`) accents with muted steel-teal borders and a softer dusty rose/orchid for accents. Lavender panels desaturated slightly. Scanlines dialed back to 7% opacity.

---

## [1.0.1] - 2026-03-03

### Added
- Theme picker button fixed to the bottom-right corner of every page. Clicking it opens a panel with five selectable themes; the choice is persisted in `localStorage` and applied on load with no flash.
  - **Terminal** (default) — dark matrix-green monospace aesthetic.
  - **Frutiger Aero** — glossy sky-blue gradients, glassy panels with backdrop-filter blur, rounded corners, Segoe UI font.
  - **DORFic Aero** — dark hewn-stone background with warm amber/copper glassmorphic panels and torchlit glow. Underground fortress meets Vista-era frosted glass.
  - **FluoroGrid** — pale sage background with muted teal grid lines, dusty lavender panels, and plum accents evoking a fluorescent-lit 80s office.
  - **NeonCubicle** — off-white with horizontal scanlines, lavender panels, cyan borders, and hot magenta accents.

### Changed
- **FluoroGrid**: Softened from pure cyan/magenta to muted teal borders and dusty plum accents for a more comfortable reading experience.
- **DORFic**: Fully redesigned as *DORFic Aero* — dark stone walls, amber glass panels, copper glow borders, parchment text.

---

## [1.0.0] - 2026-03-03

Initial release.

### Features
- Imageboard-style boards with threaded posts and image/video uploads
- Tripcodes and secure deletion tokens for anonymous users
- Admin panel with board management, post moderation, and ban system
- Rate limiting and CSRF protection
- Configurable via `settings.toml` or environment variables
- SQLite backend with connection pooling
- Nginx and systemd deployment configuration included
