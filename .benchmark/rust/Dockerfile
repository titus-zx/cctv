# syntax=docker/dockerfile:1
FROM rust:1.99-slim-bookworm AS builder
ARG WASM_BINDGEN_VERSION=0.2.105
WORKDIR /build
RUN rustup target add wasm32-unknown-unknown aarch64-unknown-linux-musl
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && curl --fail --location --proto '=https' --tlsv1.2 \
      "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/${WASM_BINDGEN_VERSION}/wasm-bindgen-${WASM_BINDGEN_VERSION}-aarch64-unknown-linux-musl.tar.gz" \
      -o /tmp/wasm-bindgen.tar.gz \
    && test "$(sha256sum /tmp/wasm-bindgen.tar.gz | cut -d ' ' -f1)" = "26bec5b1e7ba80e0fd65e5d9f4c88c304f04d9c08d2a4da32ab31af8e2fee8eb" \
    && tar -xzf /tmp/wasm-bindgen.tar.gz -C /tmp \
    && cp "/tmp/wasm-bindgen-${WASM_BINDGEN_VERSION}-aarch64-unknown-linux-musl/wasm-bindgen" /usr/local/bin/wasm-bindgen \
    && rm -rf /tmp/wasm-bindgen* /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY src src
COPY ui ui
COPY public public
# COPY can restore old timestamps; rebuild workspace packages and cache dependencies only.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo clean --release -p cctv --target aarch64-unknown-linux-musl \
    && cargo clean --release -p cctv-ui --target wasm32-unknown-unknown \
    && CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$(rustc --print sysroot)/lib/rustlib/aarch64-unknown-linux-gnu/bin/rust-lld" \
      cargo build --locked --release --target aarch64-unknown-linux-musl \
    && cargo build --locked --release -p cctv-ui --target wasm32-unknown-unknown \
    && wasm-bindgen --target web --out-dir public/pkg target/wasm32-unknown-unknown/release/cctv_ui.wasm \
    && cp target/aarch64-unknown-linux-musl/release/cctv /build/cctv

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
      ffmpeg rclone ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /build/cctv ./cctv
COPY --from=builder /build/public ./public
ENV HOST=0.0.0.0 PORT=3380 TZ=Asia/Jakarta \
    CONFIG_PATH=/app/runtime/config.json RCLONE_CONFIG=/app/rclone/rclone.conf
USER 1000:1000
EXPOSE 3380
CMD ["/app/cctv", "serve"]
