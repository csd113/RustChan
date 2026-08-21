# RustChan branding

RustChan's visual identity combines imageboard threads, terminal tooling, and a warm rust palette without reproducing the Rust programming language logo or any existing character artwork.

## Logo concept

The icon is a rounded thread card containing a terminal prompt. The speech-card silhouette represents boards and replies; the prompt represents RustChan's compact, operator-friendly server. A single status dot gives the mark a recognizable asymmetry at favicon size.

Use the SVG assets when possible. PNG exports are provided for tools that do not support SVG.

| Asset | Use |
|---|---|
| `assets/branding/rustchan-logo-dark.svg` | Full wordmark on charcoal or other dark backgrounds |
| `assets/branding/rustchan-logo-light.svg` | Full wordmark on cream, white, or other light backgrounds |
| `assets/branding/rustchan-mark-dark.svg` | Icon on dark backgrounds |
| `assets/branding/rustchan-mark-light.svg` | Icon on light backgrounds |
| `assets/branding/rustchan-logo-dark.png` | Raster export of the dark-background wordmark |
| `assets/branding/rustchan-logo-light.png` | Raster export of the light-background wordmark |
| `assets/branding/rustchan-mark-dark.png` | 512 px raster icon for dark backgrounds |
| `assets/branding/rustchan-mark-light.png` | 512 px raster icon for light backgrounds |

Keep clear space around the icon equal to roughly one terminal-stroke width. Do not stretch, rotate, add effects, recolor individual elements, or place the dark wordmark on a light surface (and vice versa).

## Mascot: Rin Rivet

Rin Rivet is RustChan's original adult hacker/sysadmin mascot. Her practical utility jacket, rugged terminal laptop, thread-card hair clip, and wrench-like crab charm connect the character to self-hosting, imageboard threads, and Rust without imitating an existing mascot or anime character. Her expression should remain confident, mischievous, and friendly; her presentation should remain suitable for a serious open-source project.

| Asset | Use |
|---|---|
| `assets/branding/rin-rivet.png` | Primary full-body transparent illustration |
| `assets/branding/rin-rivet-chibi.png` | Simplified transparent variation for smaller placements |

Both mascot files are 1024 × 1536 RGBA PNGs with transparent backgrounds. They were created specifically for RustChan without third-party artwork or external visual references.

## Color palette

| Name | Hex | Role |
|---|---|---|
| Rust | `#E86F2D` | Primary mark, hair, and brand accent |
| Ember | `#FF9A4A` | Highlights and active details |
| Charcoal | `#17191F` | Dark surfaces and mascot clothing |
| Ink | `#0D0F14` | Deep backgrounds and outline contrast |
| Warm cream | `#F2E2C4` | Light surfaces and dark-mode text |
| Copper | `#9B4D2C` | Rules and secondary accents |
| Terminal blue | `#2F80ED` | Rin's eyes and rare status accents |
| Signal blue | `#70D7FF` | Eye highlights; use sparingly |

The orange, charcoal, and cream colors carry the core brand. Blue is a small character/status accent, not a replacement primary color.

## Typography

The wordmark uses a heavy system monospace concept to echo terminal output and imageboard UI. In surrounding text, prefer readable system fonts; do not force a novelty display face for documentation body copy.
