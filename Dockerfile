# syntax=docker/dockerfile:1

# ---- Builder ------------------------------------------------------------
# Only the `server` binary is built into the runtime image. The `cli`
# ingestion tool runs earlier, in CI, and its output (the /dist directory
# and manifest.json) is what actually gets committed and deployed here.
FROM rust:1-bookworm AS builder
WORKDIR /build

# Cache dependency compilation separately from source changes: copy just
# the manifests first, stub out the two binaries, build, then overwrite
# with real source. This means editing gemtext.rs alone doesn't force a
# full crates.io dependency rebuild on every image build.
COPY Cargo.toml ./Cargo.toml
COPY cli/Cargo.toml ./cli/Cargo.toml
COPY server/Cargo.toml ./server/Cargo.toml
RUN mkdir -p cli/src server/src \
    && echo "fn main() {}" > cli/src/main.rs \
    && echo "fn main() {}" > server/src/main.rs \
    && cargo build --release -p server \
    && rm -rf cli/src server/src

COPY cli ./cli
COPY server ./server
# Touch the sources so cargo doesn't reuse the stub's stale build output.
RUN touch server/src/main.rs && cargo build --release -p server

# ---- Runtime --------------------------------------------------------------
# debian-slim rather than scratch/alpine: rustls's "ring" crypto backend
# links against glibc, and a plain glibc base keeps this Dockerfile simple
# and portable. The image is still tiny - just the binary, CA certs (for
# outbound TLS, unused at runtime but harmless), and the generated site.
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /build/target/release/server ./server
COPY dist ./dist

ENV DIST_DIR=/app/dist
ENV LISTEN_PORT=1965
EXPOSE 1965/tcp

ENTRYPOINT ["/app/server"]
