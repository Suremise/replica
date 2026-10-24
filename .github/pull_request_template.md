## What and why

<!-- What this changes, and the problem it solves. Link the issue: "Fixes #12". -->

## How to check it

<!-- What a reviewer can run or try to see it working. -->

## Checklist

- [ ] `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and `cargo deny check` pass
- [ ] New dependencies pass the licence rule (MIT or equally permissive, no copyleft: see [CONTRIBUTING.md](https://github.com/Suremise/replica/blob/main/CONTRIBUTING.md#dependencies-the-licence-rule)), or there are none
- [ ] Tests use temporary folders only, and a fix has a test that fails without it
- [ ] New user-visible text is in `locales/en/`, not in the code
- [ ] Docs updated where they're affected (`docs/cli.md`, `docs/cli-json.md`, `docs/settings.example.toml`), and an `Unreleased` entry in `CHANGELOG.md` for anything a user would notice
