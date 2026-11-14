## What changed

<!-- What this pull request does and why. Link the issue it fixes, if any. -->

## How I checked it

<!-- Commands you ran, what you tried by hand. A screenshot helps for UI changes. -->

## Checklist

- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test` passes
<!-- Without CEF set up, add `--no-default-features` to both, as CI does. -->
- [ ] If `relay/` or `phone/` changed: the relay's `tsc` and `vitest`, and the phone tests pass (see CONTRIBUTING.md)
- [ ] One fix or feature per pull request
- [ ] The title and every commit message follow [Conventional Commits](https://github.com/arakunhq/arakun/blob/main/CONTRIBUTING.md#pull-requests) (CI checks them)
- [ ] I have signed the [CLA](https://github.com/arakunhq/arakun/blob/main/CLA.md) (the bot asks on your first pull request; reply with the sentence it gives)
