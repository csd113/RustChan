# Get help with RustChan

Start with [setup](SETUP.md) if you are downloading or running RustChan for the
first time. For a specific task, use the [guide list](README.md#documentation).
RustChan is software you run yourself; this repository is not a hosted service
or an operations help desk for independent sites.

## Report a project problem

Search [existing issues](https://github.com/csd113/RustChan/issues) first.
If your problem is new, use **Report an issue** or a blank issue where available.
The form asks for your version, platform, and what went wrong. It can also be
used to explain a documentation problem or request.

Include enough detail to reproduce the problem:

- RustChan version or source commit, operating system, and processor type.
- How you installed it: native download, Docker, or source build.
- What you did, what you expected, and what happened instead.
- Relevant settings and a short log excerpt, with private information removed.

For technical problems, also mention custom Cargo features and whether the issue
involves uploads, the database, backups, a proxy, HTTPS, Tor, or moderation. A
small example using disposable data is more useful than a real database dump.
There is no guaranteed response time or individual support entitlement.

## Keep private information out of reports

Do not attach databases, backups, private posts/media, passwords, cookies, tokens,
API keys, TLS keys, Tor identity keys, identifying logs, or sensitive configuration.
For a possible security vulnerability, read [SECURITY.md](SECURITY.md) before
posting anything publicly.

## Problems on someone else's site

Contact that site's operator for content, rules, bans, appeals, harassment,
takedowns, or other abuse reports. Do not post illegal content or links to it in
project issues. The RustChan project does not operate, endorse, monitor, or
control independent sites and cannot moderate them or recover their data.

Operators are responsible for their content, moderation, users, infrastructure,
network boundaries, secrets, backups, Tor identity, availability, and applicable
law. RustChan does not guarantee anonymity, perfect security, legal compliance,
uninterrupted service, or protection from a malicious operator.

For a deployment-specific problem, check permissions, firewall/proxy settings,
and third-party services, then try to reproduce it locally with synthetic data.
The operator or service provider may need to resolve configuration and
infrastructure problems outside RustChan itself.
