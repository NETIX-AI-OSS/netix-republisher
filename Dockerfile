# NETIX Republisher web daemon (republisherd) — hardened, minimal image.
#
#   docker build -t netix-republisher .
#   docker run -p 8080:8080 -v republisher-data:/data \
#     -e REPUBLISHER_ADMIN_PASSWORD=change-me netix-republisher
#
# The final image is distroless/static (no shell, no package manager, nonroot)
# holding one fully static musl binary with the web GUI embedded. BACnet
# broadcast discovery needs host networking (see docker-compose.bacnet.yml);
# Modbus TCP and OPC UA work on ordinary bridge networking.

# ---- build stage -----------------------------------------------------------
FROM rust:1-alpine AS build

# build-base/cmake/perl/linux-headers: required to compile the rustls crypto
# provider (aws-lc-sys); git: cargo fetches the netix-protocol-core crates.
RUN apk add --no-cache build-base cmake perl linux-headers git

WORKDIR /src
COPY . .

# Build a fully static binary for the *native* platform of this build stage
# (CI runs one build per architecture on native runners). Passing --target
# explicitly keeps RUSTFLAGS off build scripts/proc-macros.
RUN set -eux; \
    target="$(rustc -vV | sed -n 's/host: //p')"; \
    rustup target add "$target"; \
    RUSTFLAGS="-C target-feature=+crt-static" \
      cargo build --locked --release \
      --no-default-features --features web \
      --bin republisherd --target "$target"; \
    install -D "target/$target/release/republisherd" /out/republisherd; \
    # Pre-create the config volume mountpoint owned by distroless nonroot.
    install -d -o 65532 -g 65532 /out/data

# ---- runtime stage ---------------------------------------------------------
FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=build /out/republisherd /usr/local/bin/republisherd
COPY --from=build --chown=65532:65532 /out/data /data

ENV REPUBLISHER_BIND=0.0.0.0:8080 \
    REPUBLISHER_CONFIG=/data/config.toml

EXPOSE 8080
VOLUME ["/data"]
USER nonroot

# No shell/curl in the image: the binary probes itself.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["/usr/local/bin/republisherd", "healthcheck"]

ENTRYPOINT ["/usr/local/bin/republisherd"]
