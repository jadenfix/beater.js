# temp.js Agent Migration Queue

Use this file as startup context for coding agents working in this repo. The
Tempera ecosystem source of truth is the top coordination repo,
`jadenfix/ecosystem`, especially `AGENTS.md`, `ecosystem.toml`, and
`docs/ecosystem-pipeline.md`.

## Product Direction

- Canonical product name: `temp.js`
- Current repo: `jadenfix/temp.js`
- Current local checkout path may still be `beater.js` until an explicit
  filesystem rename migration lands.
- Keep this repo Rust-first for runtime authority, embedding boundaries,
  protocol/state machines, durability, and security-sensitive paths. Use
  TypeScript, Python, or another language only where it is the better runtime
  surface and the boundary is explicit and tested.
- Backward compatibility is not required pre-adoption. Prefer one complete
  breaking migration over compatibility aliases when it improves ecosystem
  uniformity.

## Commands

Use the verification commands declared for `repos.beater-js` in the root
`ecosystem.toml`:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test
```

Add JS or Python smoke commands there when runtime surfaces become release
contracts.

## Contracts

The public contract is the agent runtime bridge plus generated access surfaces
such as MCP/OpenAPI adapters. Do not hand-edit generated contract artifacts.

## Storage

Use SQLite or equivalent embedded local state for runtime durability unless a
scale path is proven. Follow the root ecosystem storage split before adding any
server database.

## Ecosystem Migration Tasks

Delete each item only after it is fully migrated and verified in this repo.

- [ ] Keep `rust-toolchain.toml` on `1.96.1` with `rustfmt` and `clippy`, and
  keep workspace `rust-version = "1.96"`.
- [ ] Keep `rustfmt.toml` at `style_edition = "2024"` and run
  `cargo fmt --all`.
- [ ] Keep workspace package metadata aligned with `jadenfix/temp.js`,
  Apache-2.0 licensing, Rust 2024, and the root Tempera ecosystem manifest.
- [ ] Add uniform workspace lints: `unsafe_code = "forbid"`,
  `unwrap_used = "deny"`, and `expect_used = "deny"`; add member
  `lints.workspace = true` where needed.
- [ ] Review git dependencies on sibling ecosystem repos. Prefer published crate
  versions or explicit pinned revisions with a documented update process; do not
  switch to local `../` paths as defaults.
- [ ] Normalize any JS-facing package metadata around npm, ESM for authored
  packages, Active LTS Node for tools, and explicit generated/runtime exceptions
  where CommonJS is required.
- [ ] Migrate user-facing product naming from beater.js to temp.js across docs,
  packages, binaries, fixtures, and generated clients. Keep checkout paths
  unchanged until an explicit filesystem rename is requested.
- [ ] Verification before deleting this queue: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test`, and any
  JS/Python runtime smoke checks affected by the migration.
