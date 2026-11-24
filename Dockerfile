# Build stage. bookworm (glibc 2.36) matches the distroless cc-debian12 runtime;
# trixie's glibc is too new for it.
FROM rust:1.91-slim-bookworm AS builder

WORKDIR /src

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
# core's build.rs embeds the catalogs, so the data has to be here too.
COPY data ./data

RUN cargo build --release -p vagrant-registry-server

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder --chown=nonroot:nonroot /src/target/release/vagrant-registry-server /app/vagrant-registry-server

EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=5s --start-period=5s --retries=3 CMD ["/app/vagrant-registry-server", "--healthcheck"]

ENTRYPOINT ["/app/vagrant-registry-server"]
