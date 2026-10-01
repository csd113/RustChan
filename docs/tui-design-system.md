# RustPost / RustChan operator console design system

This is the concrete RustChan implementation contract for reproducing the suite in RustPost. Share conventions, not a compile-time dependency. The adapter supplies each application's real snapshots and actions.

## Structure and layout

- Persistent product/version/site header, explicit sampling/attention status and uptime.
- Six primary destinations: **1 Overview, 2 Tasks, 3 Content, 4 Logs, 5 System, 6 Configuration**. Content is application-specific. Account/moderation summaries currently live in System; add a dedicated destination only when its workflow warrants it.
- Header is two rows, navigation two rows, optional one-row notice, flexible body and two-row contextual footer. Keep these in that order.
- `?` opens help over the originating screen and returns there on Esc. Preserve underlying navigation context.
- Restrained, rounded single-line panels with one-column inner horizontal padding. Gray borders, colored/bold titles; no decorative icons or animations beyond actual operation progress.
- Shared rendering primitives live in `console/widgets.rs`; separate input state, validated forms, action execution, sampling, read-only task models and screen renderers.

## Keys and actions

| Key | Meaning |
| --- | --- |
| 1–6 | Primary destinations |
| Tab / Shift-Tab | Next / previous destination; form field focus inside forms |
| Up/Down, J/K | Selection or scroll; no list wrapping |
| PgUp/PgDn | One viewport page (document pages use a small fixed step) |
| Home/End | First/last row or document; End also resumes log follow |
| Enter | Read-only selected-item detail; advance form field |
| Esc | Close modal/help; clear text filter first, then return to Overview |
| / | Edit literal case-insensitive text filter on Tasks/Content/Logs |
| S | Cycle task state, content sort or log severity filter |
| P / F | Pause / follow logs |
| Left/Right | Horizontal log pan; move text cursor inside forms |
| ? / H | Contextual keyboard help |
| R | Request a new background snapshot |
| C / A / D or X | RustChan: create board / create administrator / select thread for deletion |
| Q | Confirm graceful server shutdown (Q closes help/details) |
| Ctrl-C | Immediate shutdown, including below minimum terminal size |
| Ctrl-U | Clear active text editor |
| Ctrl-Enter / F2 | Validate and submit a complete form |

Search owns all printable shortcuts until Enter applies or Esc clears/closes it; accept at most 120 Unicode scalar values and strip controls/paste newlines. Held or released confirmation keys never perform a destructive action. In normal navigation, repeat text events only reach scrolling handlers.

## Status and feedback

Color supplements explicit words; all states remain intelligible in monochrome.

| Meaning | Treatment |
| --- | --- |
| Live / healthy | Green `[LIVE]` / `[OK]` |
| Active task | Cyan `RUNNING` / `[ACTIVE]` |
| Idle | Muted `[IDLE]` |
| Waiting / queued / stale / attention | Yellow `QUEUED`, `[WAIT]`, `[STALE]`, `[ATTENTION]` |
| Completed | Green `COMPLETED` |
| Error / failure / unavailable | Red `FAILED`, `[ERROR]`, `[DEGRADED]` |
| Disabled / unknown | Muted explicit `DISABLED` / `UNKNOWN` |
| Selected row | Dark neutral background (`35,48,55`), bold foreground and `›` marker |
| Focus / neutral info | Cyan |

Success/info notices expire after eight seconds. Errors persist until an intentional navigation/refresh action replaces them. A failed collector reports unavailable data, never healthy zero counts. Header sampling health includes database/operator failures and known job/media failures; database availability is explicitly a successful query, not an integrity check.

## Tables and details

- Tasks: running/queued first, then recent failures/completions; maximum 40 rows per durable state. Aggregate counts cover the authoritative queue, not just visible rows. Show an omission indicator when capped.
- Preserve selection by stable identity on refresh/reordering. Clamp rows and paging after filtering/resizing/deletion. Filtering must have a visible query and mode indicator.
- Content: name ascending, threads descending or posts descending, with deterministic name ties. Table virtualization keeps the selected row visible.
- Enter opens scrollable, labeled details. Wide layouts may show a secondary selected-item panel; full details must remain available on narrower terminals.
- Never display raw task payloads, hashes, tokens or credential structures. System lists only the newest 25 administrator identities alongside the full count; web admin owns complete account management.

## Tasks and metrics

Use Queued, Running, Completed and Failed for the actual persisted states. Add Cancelled only when an application's tracker truly publishes it. Unknown values must stay unknown.

Display task type, nonsecret item identity, stable job ID, claim attempts, creation and last transition timestamps (UTC). Age means seconds since the persisted transition; for a running job this is the claim time, not a reliable ETA. Runtime trackers with no timestamps say “Not published”. Backup progress uses actual file/byte counters during an active creation phase. Do not infer success/failure from a stale backup phase or offer cancellation/retry through a process-wide shutdown token.

Overview shows live request totals/rate/in-flight count, recently active IPs (not user sessions), content/storage/memory, queue/media summary and attention counts. Traffic history retains at most 120 measured intervals, each at least ten seconds. Sparklines represent requests per observed interval; rates use actual elapsed durations. Show current/average/peak requests per second and total observed duration. Manual refreshes must not fabricate extra samples. Empty history has a warmup message.

Sample database/process state on a separate task every three seconds; manual refresh resets that interval. Upload-tree size is cached thirty seconds and failures are unavailable. Input/drawing never perform a storage-tree scan. Memory/history/task/log buffers must remain bounded.

## Logs

Retain at most 256 KiB / 4,000 lines from the active main-process log, excluding the dependency log. Cap each line, remove controls and redact named credential fields and URL userinfo before buffering/search/inspection. Text and severity filters compose. Parse the severity field rather than matching words in the message. Pausing freezes the buffered view; F follows again. Enter inspects the last visible record. Display source, follow/paused state and retained-buffer limits; retention is not the complete log archive.

## Dialogs, configuration and safety

- Dim the background; clear a centered popup; clamp width/height to the terminal. Details scroll with exact Unicode-aware bounds.
- Forms show purpose, focused field help, inline validation, masked secrets and submit/cancel keys. Long forms scroll to the focused field.
- Selecting deletion opens an ID form, then a separate red confirmation requiring a fresh Y. Enter cannot confirm thread deletion. Progress prevents duplicate operations.
- Configuration is an explicit nonsecret allowlist with **read only**, **runtime effective**, **startup / restart required** labels and source/override information. Changing a runtime value must use the application's guarded command path with clear outcome/confirmation; no renderer writes configuration.
- Preserve first-run setup, terminal cleanup on normal/error/panic paths, no-TTY service behavior and existing business validation.

## Responsive behavior and empty states

- Minimum: 44 columns × 14 rows. Below that, show current/minimum size and resize/Ctrl-C instructions; hidden forms/confirmations reject input.
- Below 58 columns, abbreviate navigation and retain `?Help`. Below 76, shorten footer hints.
- Overview stacks below 100 columns; label values wrap and all sections remain reachable by scrolling. At larger widths, operations and access panels sit side by side.
- Content drops its side detail below 78 columns. Tasks drop the side detail below 110 and use compact two-column rows below 68 columns of table width. Enter still opens complete details.
- System/configuration/details wrap at complete grapheme boundaries and scroll. Never hide a critical value permanently because a panel is short.
- Every surface deliberately describes loading, empty/no matching rows, unavailable data or failure. Preserve filters and selection on ordinary resize. Test tiny/zero sizes, long Unicode names, secret masking, query failure and recovery.
