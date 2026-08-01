## Summary

<!-- What problem does this solve, and why is this approach appropriate? -->

## Impact

<!--
List affected surfaces: public UI, admin UI, storage/schema, media, moderation,
backup/restore, TLS/proxy, Tor, ChanNet, deployment, or documentation.
State "None" where appropriate.
-->

## Validation

<!-- List exact commands run, focused tests, and results. -->

## Checklist

- [ ] This change is narrowly scoped and preserves unrelated behavior.
- [ ] I have not included credentials, private keys, databases, backups, logs,
      private media, personal information, sensitive instance data, runtime
      data, or large generated artifacts.
- [ ] Tests cover changed behavior and relevant failure paths, or I have
      explained why no test applies.
- [ ] Formatting, Clippy, Rust tests, and/or focused Playwright checks were run
      as appropriate to the change.
- [ ] Documentation and the `CHANGELOG.md` `indev` section were updated when
      behavior, configuration, deployment, or operator expectations changed.
- [ ] Schema changes use a forward migration and cover fresh, upgrade, failure,
      and backup/restore behavior where applicable.
- [ ] UI changes preserve route names, IDs, form fields, JS/CSS hooks, and no-JS
      fallbacks unless the change intentionally updates them.
- [ ] Security-sensitive details have been coordinated under `SECURITY.md` and
      are safe to publish.

## Related issue

<!-- Use "Closes #123" when appropriate. -->
