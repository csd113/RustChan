# RustChan console audit

## Existing interface

The console enters Crossterm raw/alternate-screen mode only for interactive stdin/stdout. Tokio owns input dispatch, sampling and rendering; a dedicated input thread delivers typed events. Ratatui handles Unicode and terminal diffs. Noninteractive logging, first-admin setup, signal shutdown and terminal restoration must survive the redesign.

Four destinations existed: Overview (listeners, Tor, request counters, online IPs, content/storage/memory and aggregate media work), Boards (counts, selection, paging, contextual counts), Logs (256 KiB disk tail, severity emphasis, pause by scrolling, follow and horizontal pan), Help. Administrative forms create boards with image/video/audio/NSFW policies, create masked administrator credentials and delete a thread after a separate destructive confirmation. R refreshes; Q confirms graceful shutdown; Ctrl-C stops immediately. These capabilities are retained.

`state.rs` owns validated forms and modal transitions; `wizard.rs` executes operations off the input thread. `dashboard.rs` combined all screens, dialogs, formatting, styles, log loading and visual tests in about 2,000 lines. Preserve tested input/forms; split presentation by concern and extend typed navigation.

## Available authoritative runtime data

- Request/in-flight/upload atomics and recently active IP map. IP count is not a session or user count.
- SQLite board/thread/post counts and per-board counts; database pages and upload-tree size; platform RSS.
- Durable `background_jobs`: video transcodes, audio waveforms, required thread pruning, spam checks; pending/running/done/failed states, priority, attempts, creation and transition times. Payloads contain private IP hashes; never display raw payloads.
- Existing background-job summary counts (including unacknowledged failures and completions in 24 hours), queue rejection counter, active video counter.
- Backup phase/file/byte counters, maintenance gate label and database maintenance status/phase/report.
- Startup FFmpeg/WebP/VP9/Opus capability detection; effective listener port; live onion address.
- Admin account identities; open reports/appeals and active bans; pending/failed media state; moderation log events.
- Configuration has both boot values and runtime settings (FFmpeg timeout, automatic backups). Only explicitly selected nonsecret fields may be rendered.

## Boundaries

Workers do not publish percentage, ETA, throughput or worker identity. Their cancellation token belongs to server shutdown; it is not safe per-job cancellation. Retrying a durable job requires guarded media/state transactions, so expose diagnostics and existing web-admin workflows rather than writing queue rows from the TUI. Completed job rows may be pruned by existing maintenance. Backup counters do not provide a failure state or start time. Short scheduled cleanup operations lack lifecycle trackers. Do not label any of these missing values as zero/success or infer a fabricated task.

## Redesign structure

Overview, Tasks, Content, Logs, System and Configuration share a compact header/navigation/footer. Contextual help is an overlay that returns to its originating screen. Tasks combine bounded durable rows with tracked maintenance/backup work. Overview shows attention and bounded traffic history. Search, sort, detail overlays, stable identity and paging apply to lists; logs gain level/text filters and deliberate pause/follow. Preserve the existing administration forms and confirmations. Document concrete reusable conventions for RustPost in `tui-design-system.md`.
