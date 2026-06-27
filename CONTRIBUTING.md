# Contributing to NETIX Republisher

## Development setup

1. Install Rust stable (`rustup default stable`).
2. Clone [netix-protocol-core](https://github.com/NETIX-AI-OSS/netix-protocol-core) alongside this repo.
3. Override protocol crates locally in `Cargo.toml` with `[patch]` path entries when developing both repos together.

## Verify changes

```bash
cargo fmt --all -- --check
cargo clippy --all-targets
cargo test --locked
cargo build --release
```

## Sample config

```bash
cargo run --bin write_config
```

Writes `config.toml` with BACnet simulator-aligned sample points.
