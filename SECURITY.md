# RustChan security

Keep administrator credentials, site data, backups, TLS keys, and Tor identity
keys private. For public hosting, read the [deployment guide](docs/deployment.md).
If you believe you found a security bug, do not post sensitive details publicly.

[All guides](README.md#documentation) · [Support](SUPPORT.md) · [Backups](docs/backups.md)

## Private reporting status

As checked on 2026-11-05, GitHub private vulnerability reporting is disabled for
this repository. The project publishes no verified security email or other
private inbound channel. There is currently no project-controlled private route
for an outside reporter to send sensitive vulnerability details.

Keep your report private and check this policy and the
[repository Security page](https://github.com/csd113/RustChan/security) for a
future private channel. Do not put a vulnerability, exploit, credentials, keys,
database contents, private media, IP addresses, or identifying site data in a
public issue, discussion, PR, or commit.

Maintainers need to enable GitHub private reporting or publish a verified private
contact, then update these instructions. This policy does not promise a fixed
security-support window, response time, remediation date, or release date.
Reports are assessed against the current tree and affected releases case by case.

## What to report

Report a defect in RustChan code or project-owned configuration with a concrete
security impact. Examples include:

- Authentication, session, CSRF, authorization, or admin-boundary bypasses.
- Unsafe uploads, media processing, paths, archives, backups, restores, or database operations.
- Exposure or replacement of credentials, private content, backups, or Tor identity keys.
- Request parsing, proxy, HTTPS, or Tor defects affecting confidentiality, integrity, or availability.
- Injection, traversal, cross-site scripting, remote code execution, or meaningful denial of service.
- Dependency vulnerabilities reachable through a supported RustChan build or runtime path.

Operator misconfiguration, exposed credentials, unsafe proxy rules/permissions,
missing backups, unsupported changes, and a malicious operator controlling their
own server are generally outside that scope. So are content or moderation disputes
on independent sites and hardening suggestions without a demonstrated defect.

### Prepare a private report

Once a verified private channel exists, include the affected version/commit,
platform, installation method, relevant features/configuration, reproducible
steps, expected and actual results, and security impact. Explain which part of
the system is affected and include a minimal proof of concept or sanitized logs
when useful.

Use a disposable local installation and synthetic data. Remove credentials,
tokens, cookies, passwords, API keys, TLS/Tor keys, IP addresses, database contents,
private posts/media, hostnames, and other identifying information before sending.
Do not probe a site without the owner's permission.

## Operational security

The following is a reference for operators. RustChan's protections do not promise
anonymity, perfect security, uninterrupted availability, legal compliance, or
protection from an operator who controls the server. Operators remain responsible
for their instances, moderation, content, infrastructure, backups, and secrets.

### Accounts and network access

Run as an unprivileged account. Administrators have full privilege; there is no
limited moderator role. Passwords use Argon2id, client identities are hashed,
and admin actions require authenticated sessions with scoped signed CSRF and
origin checks. These controls do not prevent operators from observing or changing
their own site. Keep the generated cookie secret stable and private.

Public web deployments need HTTPS and a protected backend. Trust forwarded client
headers only from configured proxy CIDRs. Proxies must remove chunked transfer
encoding and forward `Content-Length`; the app rejects `Transfer-Encoding`.
Keep detailed readiness and metrics behind a trusted monitoring boundary.
See [proxy guidance](docs/deployment.md#reverse-proxy).

### Files and recovery

RustChan validates upload/backup paths, media, archive inventories, and restored
database structures before publication. That does not mean every original upload
has had metadata removed. An optional per-board
[strict metadata policy](docs/media-capabilities.md#strict-metadata-safe-uploads-optional)
requires supported new images to be re-encoded and rejects unsupported or failed
sanitization. It does not certify historical media, remove identifying image
content or guarantee anonymity; see [media limits](docs/media-capabilities.md#image-and-metadata-limits).

Keep recovery journals and snapshots after failed restores or older update
transactions. For rollback, restore a complete stopped backup with its matching
executable. Application backups omit some runtime/external persistent paths;
see [backups](docs/backups.md) and [updates](docs/software-updates.md).

### Resource limits

Request, socket, client-identity-table, search, and media-work bounds reduce abuse;
they do not guarantee availability under distributed traffic. Per-client fixed
windows reset on restart. Shared NAT and Tor identities affect fairness.
Search deadlines are cooperative and can be exceeded between SQLite checkpoints.

Native media libraries run in-process without a process sandbox or universal
forced cancellation. There is no universal disk quota or hard log-byte retention
ceiling. Monitor disk space, queues, and retention. Catalog/preference collections
cap at 1,000 threads; oversized legacy boards still have direct thread and index
navigation. See [media resource boundaries](docs/media-capabilities.md#resource-boundaries).

### Dependencies and updates

Advisory exceptions live in [deny.toml](deny.toml) and [.cargo/audit.toml](.cargo/audit.toml).
Recheck the Arti-related RSA vulnerability and `paste` maintenance exceptions
when upgrading Arti. An ignored advisory is not a permanent resolution. Patch
FFmpeg and the host/container environment independently.

The admin update checker supplies official release links. Operators verify the
download and perform upgrades with complete stopped backups. It does not install
or restart the app.

## Public follow-up

After resolution or coordinated disclosure, a sanitized public issue or PR may
be appropriate. Do not expose a working exploit against unfixed deployments or
operator/user data. Maintainers may credit reporters, publish an advisory, or
request more validation; no particular disclosure process or timeline is promised.
