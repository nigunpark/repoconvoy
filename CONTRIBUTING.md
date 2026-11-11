# Contributing

RepoConvoy is an early Rust project. Open an issue to discuss a larger feature before implementing it. Keep changes scoped to a demonstrable user action; do not replace an unimplemented worker or review flow with fake status data.

Before a PR, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all-targets`. If the browser UI changed, also run `npm ci`, `npx playwright install chromium`, and `npm run test:ui` (Node is test-only). Use disposable Git repositories in tests. Never commit real repository contents, credentials, generated `target/` output, or local state databases. Describe what was actually exercised and what remains unverified.

Report security issues privately as described in `SECURITY.md`.
