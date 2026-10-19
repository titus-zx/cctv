# syntax=docker/dockerfile:1
FROM rust:1.99-slim-bookworm AS builder
ARG WASM_BINDGEN_VERSION=0.2.105
WORKDIR /build
# YOLOv8n COCO ONNX, pinned to a source revision and verified before inclusion.
ADD https://raw.githubusercontent.com/Hyuto/yolov8-onnxruntime-web/fc4a52c466d15ad4519873a0cef22fbc935b93b6/public/model/yolov8n.onnx /build/models/yolov8n.onnx
RUN echo "505648ada344cd9f3f31e51d49c489c070819bc96cc758258a8fd51488e00579  /build/models/yolov8n.onnx" | sha256sum -c - \
    && chmod 644 /build/models/yolov8n.onnx
RUN rustup target add wasm32-unknown-unknown x86_64-unknown-linux-musl
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates musl-tools \
    && curl --fail --location --proto '=https' --tlsv1.2 \
      "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/${WASM_BINDGEN_VERSION}/wasm-bindgen-${WASM_BINDGEN_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
      -o /tmp/wasm-bindgen.tar.gz \
    && test "$(sha256sum /tmp/wasm-bindgen.tar.gz | cut -d ' ' -f1)" = "b391448c4926ac4b11425a6752484d85164e72489d97804461d5e868c643b88a" \
    && tar -xzf /tmp/wasm-bindgen.tar.gz -C /tmp \
    && cp "/tmp/wasm-bindgen-${WASM_BINDGEN_VERSION}-x86_64-unknown-linux-musl/wasm-bindgen" /usr/local/bin/wasm-bindgen \
    && rm -rf /tmp/wasm-bindgen* /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY src src
COPY ui ui
COPY public public
# COPY can restore old timestamps; rebuild workspace packages and cache dependencies only.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo clean --release -p cctv --target x86_64-unknown-linux-musl \
    && cargo clean --release -p cctv-ui --target wasm32-unknown-unknown \
    && CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld" \
      cargo build --locked --release --target x86_64-unknown-linux-musl \
    && cargo build --locked --release -p cctv-ui --target wasm32-unknown-unknown \
    && wasm-bindgen --target web --out-dir public/pkg target/wasm32-unknown-unknown/release/cctv_ui.wasm \
    && cp target/x86_64-unknown-linux-musl/release/cctv /build/cctv

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
      ffmpeg rclone ca-certificates tzdata intel-media-va-driver \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /build/cctv ./cctv
COPY --from=builder /build/public ./public
COPY --from=builder /build/models ./models
ENV HOST=0.0.0.0 PORT=3380 TZ=Asia/Jakarta \
    CONFIG_PATH=/app/runtime/config.json RCLONE_CONFIG=/app/rclone/rclone.conf
USER 1000:1000
EXPOSE 3380
CMD ["/app/cctv", "serve"]
