# RustChan Support

RustChan is self-hosted imageboard software written in Rust. This repository
provides project documentation and issue tracking; it is not a hosted RustChan
service or an operations help desk for independent instances.

## Start here

- Read [README.md](README.md) for the project overview and quick start.
- Read [SETUP.md](SETUP.md) for installation, configuration, Tor, TLS, reverse
  proxy, backup, update, and troubleshooting guidance.
- Search existing issues before filing a new one.
- Use the structured bug, feature, or documentation form when issue creation is
  available to your account.
- Read [SECURITY.md](SECURITY.md) before reporting a vulnerability.

When requesting project help, include the RustChan version or commit, operating
system and architecture, installation method, enabled Cargo features, relevant
deployment configuration, exact reproduction steps, and sanitized logs. State
whether the database, media processing, backup/restore, reverse proxy, TLS, Tor,
public/admin UI, moderation, or ChanNet is involved.

There is no guaranteed response time or individual support entitlement.

## Repository issues are not for

Do not use GitHub issues for:

- illegal content or links to it;
- personal information, private posts, private media, or identifying logs;
- credentials, cookies, tokens, API keys, TLS keys, onion private keys,
  databases, backups, or sensitive instance configuration;
- harassment reports, moderation appeals, ban disputes, takedown requests, or
  other abuse reports concerning an independently operated RustChan site; or
- vulnerability details that belong under [SECURITY.md](SECURITY.md).

Contact the relevant site's operator for its rules, content, moderation, and
abuse processes. The RustChan project does not operate, endorse, monitor, or
control independent instances and cannot moderate them or recover their data.

## Operator responsibility

Each operator is responsible for their instance's laws, moderation rules,
content, users, security, availability, backups, Tor/onion-service identity,
network boundaries, and infrastructure. Running RustChan does not guarantee
anonymity, perfect security, uninterrupted service, legal compliance, or
protection against a malicious operator.

For deployment-specific troubleshooting, reproduce the problem on a disposable
local instance with synthetic data before posting. If the problem occurs only
because of local permissions, proxy configuration, exposed credentials,
firewalling, unsupported changes, or third-party services, the operator or that
service's support channel is the appropriate source of help.
