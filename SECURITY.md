# RustChan Security Policy

RustChan accepts responsible reports about security defects in this repository.
This policy does not make the project responsible for independently operated
RustChan instances.

## Supported versions

RustChan does not currently publish a fixed security-support window or response
timeline. Identify the exact release, commit, and deployment configuration you
tested. Maintainers will assess the current tree and affected releases case by
case without guaranteeing remediation or release dates.

## What to report

A security report should describe a defect in RustChan code or project-owned
configuration that can affect confidentiality, integrity, authorization, or
availability. Examples include:

- authentication, session, CSRF, authorization, or admin-boundary bypasses;
- unsafe upload, media-processing, path, archive, backup, restore, or database
  behavior;
- disclosure or replacement of credentials, database contents, private media,
  backup material, or Tor/onion-service identity keys;
- request parsing, reverse-proxy, TLS, or Tor defects with a concrete
  security impact;
- remote code execution, injection, traversal, cross-site scripting, or
  meaningful denial of service; and
- dependency vulnerabilities that are reachable through RustChan's supported
  build or runtime behavior.

The following are generally not RustChan vulnerability reports:

- operator misconfiguration, exposed admin credentials, insecure proxy rules,
  missing backups, unsafe file permissions, or unsupported modifications;
- illegal content, harassment, moderation decisions, or abuse occurring only
  on an independently operated site;
- claims that rely on a malicious operator controlling their own server; or
- general hardening suggestions without a demonstrated RustChan defect.

Operators remain responsible for their instances, applicable law, moderation,
content, secrets, backups, Tor identity, network boundaries, and infrastructure.
RustChan does not promise anonymity, perfect security, uninterrupted
availability, legal compliance, or protection from a malicious operator.

## Operational security

Run RustChan as an unprivileged account and keep configuration, databases,
backups, TLS keys, and Tor identity private. Administrator accounts have full
administrative privilege; the project has no separate limited moderator role.
Passwords use Argon2id, client identities are hashed, and administrator actions
require authenticated sessions with scoped signed CSRF and origin checks. These
controls do not prevent an operator from observing or modifying their own site.

Public deployments need HTTPS and a protected backend. Trust forwarded client
headers only from configured proxy CIDRs. The application rejects
`Transfer-Encoding`; proxies must dechunk bodies and forward `Content-Length`.
Keep detailed readiness and metrics behind a trusted scrape/network boundary.
See [deployment guidance](SETUP.md#reverse-proxy-notes).

Upload/backup paths, media content, archive inventories, and restored database
structures are validated before publication. Preserve recovery journals and
snapshots after a failed restore/update. Use complete stopped data backups and
matching executables for rollback; see [software updates](docs/software-updates.md).
Application backups do not include every runtime or external persistent path.

Request, socket, identity-table, search, and media-work bounds reduce resource
abuse but do not guarantee availability against distributed traffic. Per-client
fixed windows reset on process restart; shared NAT and Tor identities affect
fairness. Search deadlines are cooperative and can be exceeded between SQLite
checkpoints. Native media libraries run in-process with no process sandbox or
universal forcible cancellation. There is no universal disk quota or hard log
byte-retention ceiling: monitor storage, job queues, and retention policies.
Catalog/preference collections cap at 1,000 threads; oversized legacy boards
still have direct thread and index navigation. Current preview limits are in
[media capabilities](docs/media-capabilities.md).

Cargo dependency advisory exceptions are maintained in
[deny.toml](deny.toml) and [.cargo/audit.toml](.cargo/audit.toml). Re-review the
Arti-related RSA vulnerability and `paste` maintenance exceptions on Arti
upgrades; do not treat an ignored advisory as permanently resolved. FFmpeg and
the host/container environment need independent patch maintenance. Signed native
update packages have their own verification contract; a signature does not
establish all deployment or power-loss recovery guarantees.

## Private reporting status

GitHub private vulnerability reporting is currently disabled for this
repository, and the project publishes no security email address or other
verified private inbound channel. Consequently, there is currently no safe
project-controlled way for an outside reporter to transmit sensitive
vulnerability details.

Do not open a public issue, discussion, pull request, or commit containing a
vulnerability, exploit, credentials, private keys, database contents, private
media, onion private keys, IP addresses, or identifying instance data. Preserve
the report privately and check this file and the repository Security page for a
future private channel.

**Maintainer action item:** enable GitHub private vulnerability reporting or
publish a verified private security contact, then replace this section with the
exact reporting instructions.

Once a private channel is available, include:

- the affected RustChan version or commit;
- operating system, architecture, installation method, enabled Cargo features,
  and relevant reverse-proxy, TLS, Tor, database, or media-toolchain
  configuration;
- minimal, deterministic reproduction steps;
- expected behavior, actual behavior, and security impact;
- whether public posting, admin access, moderation, media, backups/restores,
  storage, or Tor identity is involved; and
- sanitized logs, requests, stack traces, or a minimal proof of concept.

Before transmission, remove or replace all credentials, tokens, cookies,
passwords, API keys, TLS keys, onion private keys, IP addresses, database
contents, private posts or media, hostnames, and identifying information. Use a
disposable local instance and synthetic data whenever possible. Do not probe a
site you do not own or lack permission to test.

## Public follow-up

After a report is resolved or disclosure is coordinated, a sanitized public
issue or pull request may be appropriate. Public material must not expose a
working exploit against unfixed deployments or reveal operator or user data.
Maintainers may credit reporters, publish an advisory, or request additional
validation, but this policy does not promise a particular disclosure process or
timeline.
