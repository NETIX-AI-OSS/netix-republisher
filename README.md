# NETIX Republisher

One binary that discovers, browses, polls, and republishes points from **any**
registered industrial protocol — **BACnet/IP, Modbus TCP, OPC UA** — to MQTT/TLS.
Capability-driven iced GUI: the connection and per-point fields, discovery
controls, and browse mode are rendered from each protocol's declared
capabilities, so switching protocols swaps the UI with no hard-coded knowledge.

The protocol adapters and republisher core live in
[`netix-protocol-core`](https://github.com/NETIX-AI-OSS/netix-protocol-core) and
are consumed here as git dependencies (the exact commit is pinned in
`Cargo.lock`).

## Build & run

```bash
cargo build --release
./target/release/republisher          # iced GUI
```

Linux GUI builds need the usual windowing dev libraries (see the release
workflow for the exact apt package list).

## Releases

Tagged releases (`v*`) publish platform zip files only — for example
`netix-republisher-<tag>-linux-x86_64.zip`, `...-linux-aarch64.zip`,
`...-macos-x86_64.zip`, `...-macos-aarch64.zip`, and `...-windows-x86_64.zip`
— plus a `SHA256SUMS` checksum file and build-provenance attestation. GitHub's
auto-generated "Source code" archives are removed; extract the zip for your
platform and run `republisher` (Linux/macOS) or `republisher.exe` (Windows).

## Protocol notes

- **BACnet/IP** — Who-Is/I-Am discovery, object-list browse, ReadProperty(Multiple)
  poll. Uses a vendored, patched `bacnet-transport` (see `NOTICE`) so Who-Is
  broadcasts work from an ephemeral local port.
- **Modbus TCP** — no native discovery; manual endpoints (host/port/unit-id) and
  a register-range scan. Per-point datatype, word order, and scale are configurable.
- **OPC UA** — discovers endpoints, walks the address space recursively
  (following `BrowseNext` continuations, skipping the OPC UA core hierarchy), and
  reads node values.

## Licensing

Apache-2.0 (see `LICENSE`). OPC UA support pulls in `async-opcua` (MPL-2.0); the
BACnet adapter uses a vendored, patched `bacnet-transport` (MIT). See `NOTICE`.
