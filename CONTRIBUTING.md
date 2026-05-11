# Contributing to toki-sync

Thanks for your interest in contributing. This document covers how to set up the project, the change workflow (branch, commit, PR), and the style checks your code must pass.

For the Korean version, see [CONTRIBUTING.ko.md](CONTRIBUTING.ko.md).

## Development setup

### Prerequisites

- Rust toolchain (stable). The project targets Rust 2021 edition; the latest stable release is recommended.
- Docker and Docker Compose v2 (only required if you want to run the full container stack locally).

The repository does not pin a `rust-version` in `Cargo.toml`; any stable toolchain that supports edition 2021 should build.

### Build and run

```bash
cargo build
cargo test
```

To run the server against a local SQLite + Fjall stack, copy the example config and start the binary:

```bash
cp config/toki-sync.toml.example config/toki-sync.toml
cargo run -- --config config/toki-sync.toml
```

## Pull requests

Keep one fix or feature per PR. Before opening the PR:

- Rebase onto the latest `main`.
- Run `cargo fmt --all` and `cargo clippy --all-targets --all-features -- -D warnings`.
- Run `cargo test`.
- Write a clear description of what changed and why.

### Branch naming

Use short, kebab-case names prefixed by the change kind:

- `feat/<short-desc>` — new feature
- `fix/<short-desc>` — bug fix
- `docs/<short-desc>` — documentation only
- `chore/<short-desc>` — tooling, CI, dependency bumps
- `refactor/<short-desc>` — code restructure with no behavior change

### Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/) prefixes that match the history of this repo (`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`). Keep the subject under 72 characters and use the imperative mood ("add X", not "added X").

Examples from the existing history:

```text
feat: implement ClickHouse event store backend
fix: validate token_columns — length check + sanitize label names
docs: update deploy guides, config, .env for device code flow
```

### Sign-off (DCO)

The project does not require a CLA. Pull requests must include a Developer Certificate of Origin sign-off on every commit:

```bash
git commit -s -m "feat: add my change"
```

This appends a `Signed-off-by:` trailer that asserts you have the right to submit the patch under the project license.

## Issues

Found a bug? Have a feature idea? Open an issue using the templates in `.github/ISSUE_TEMPLATE/`. Include reproduction steps, the toki-sync version (or commit), and relevant logs.

## Code style

- `cargo fmt --all` before committing.
- `cargo clippy --all-targets --all-features -- -D warnings` must pass.
- Follow existing patterns in the codebase. When in doubt, prefer the structure of the nearest neighboring module.

## License

By contributing, you agree that your contributions are licensed under [FSL-1.1-Apache-2.0](LICENSE).
