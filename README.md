# NETIX Republisher

One codebase that discovers, browses, polls, and republishes points from **any**
registered industrial protocol — **BACnet/IP, Modbus TCP, OPC UA** — to MQTT/TLS,
with two interchangeable frontends:

- **`republisher`** — the capability-driven iced desktop GUI.
- **`republisherd`** — the same feature set as a headless daemon with an
  embedded **web GUI**, shipped as a hardened container image for edge/IoT
  deployment (`ghcr.io/netix-ai-oss/netix-republisher`).

In both, the connection and per-point fields, discovery controls, and browse
mode are rendered from each protocol's declared capabilities, so switching
protocols swaps the UI with no hard-coded knowledge.

The protocol adapters and republisher core live in
[`netix-protocol-core`](https://github.com/NETIX-AI-OSS/netix-protocol-core) and
are consumed here as git dependencies (the exact commit is pinned in
`Cargo.lock`).

## Build & run

```bash
cargo build --release
./target/release/republisher          # iced desktop GUI

cargo build --release --no-default-features --features web --bin republisherd
./target/release/republisherd         # web GUI on http://0.0.0.0:8080
```

Linux desktop-GUI builds need the usual windowing dev libraries (see the
release workflow for the exact apt package list); the web daemon needs none.

## Docker (web GUI)

```bash
docker run -p 8080:8080 -v republisher-data:/data \
  -e REPUBLISHER_ADMIN_PASSWORD=change-me \
  ghcr.io/netix-ai-oss/netix-republisher:latest
```

The image is distroless/static (no shell, nonroot, fully static musl binary,
~tens of MB) and is published multi-arch (`linux/amd64`, `linux/arm64`) with
registry-stored SBOM and build provenance. `docker-compose.yml` shows a
turnkey bridge-network deployment; **BACnet needs `network_mode: host`**
(Who-Is is a UDP broadcast that cannot cross the Docker bridge) — see
`docker-compose.bacnet.yml`.

Turnkey boot: everything can be provisioned via env so the container comes up
ready and waiting on a target device — `REPUBLISHER_ADMIN_PASSWORD` (or
`*_HASH`, argon2), `REPUBLISHER_PROTOCOL`, `REPUBLISHER_MQTT_*` broker
settings, `REPUBLISHER_CONNECTION_JSON`, `REPUBLISHER_POINTS_JSON`, and
`REPUBLISHER_AUTOSTART=true` to begin publishing immediately. Run
`republisherd --help` for the full list. Config edited in the GUI persists to
the `/data` volume; env values override it on every boot. If no admin
credential is provided, a random password is generated and printed to the
container log once. `REPUBLISHER_TLS_CERT`/`REPUBLISHER_TLS_KEY` serve the GUI
over HTTPS; otherwise put it behind your reverse proxy or keep it on a
management LAN.

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
