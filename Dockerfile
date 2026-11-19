# syntax=docker/dockerfile:1

# Use the latest stable 1.x: dependencies in Cargo.lock (e.g. icu_collections
# 2.x via url/idna) require edition2024, stabilized in Rust 1.85. Pinning an
# older toolchain here makes the cross-arch docker build fail to parse manifests.
FROM --platform=$BUILDPLATFORM rust:1-alpine AS build
ARG TARGETOS
ARG TARGETARCH
ARG TARGETVARIANT
WORKDIR /src
# Map Docker TARGETARCH to Rust target triple. reqwest uses rustls (pure Rust),
# so no native toolchain / musl-dev is needed for static musl builds.
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) T=x86_64-unknown-linux-musl;; \
      arm64) T=aarch64-unknown-linux-musl;; \
      arm)   T=armv7-unknown-linux-musleabihf;; \
      *)     T=x86_64-unknown-linux-musl;; \
    esac; \
    rustup target add "$T"; \
    echo "$T" > /tmp/rust_target
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY src ./src
RUN set -eu; \
    T=$(cat /tmp/rust_target); \
    cargo build --release --target "$T"; \
    mkdir -p /out; \
    cp "target/$T/release/matrix-satori" /out/matrix-satori

FROM alpine:3.20
RUN apk add --no-cache ca-certificates tzdata \
 && addgroup -g 10001 -S matrixsatori \
 && adduser -u 10001 -S -G matrixsatori matrixsatori
WORKDIR /app
COPY --from=build /out/matrix-satori /app/matrix-satori
RUN mkdir -p /app/data && chown -R matrixsatori:matrixsatori /app
USER matrixsatori
VOLUME ["/app/data"]
EXPOSE 5140 8088
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD wget -q -O /dev/null http://127.0.0.1:5140/healthz || exit 1
ENTRYPOINT ["/app/matrix-satori"]
