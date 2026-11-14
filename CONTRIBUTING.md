# Contributing to Arakun

Thanks for helping out. Issues, ideas and pull requests are all welcome.

## Contributor License Agreement

First-time contributors are asked to sign our [CLA](CLA.md). A bot comments on
your first pull request, and signing is a single reply:

```
I have read the CLA Document and I hereby sign the CLA
```

In short: **you keep the copyright to your work** and grant the maintainer the
right to distribute it under other terms too (for example a commercial license).
The released code stays AGPL-3.0; the CLA is what keeps dual licensing possible
without chasing every past contributor. Unsigned pull requests can't be merged.

## Before you open a pull request

- `cargo clippy --all-targets -- -D warnings` and `cargo test` must pass.
- Keep changes focused: one fix or feature per pull request.
- Describe what changed and how you checked it (a screenshot helps for UI changes).

You don't need to set labels. A workflow gives pull requests area labels
(`area: relay`, `area: ci`, ...) from the paths they change, and issues from
their title prefix (`CI:`, `Relay:`) or the issue form's Area field. The release
notes are grouped by these labels. The labels themselves live in
`.github/labels.yml`.

## License

By contributing you agree that your contribution is released under the
[GNU Affero General Public License v3.0](LICENSE), in addition to the grants in
the [CLA](CLA.md).
