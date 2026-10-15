# syntax=docker/dockerfile:1.7
# Spec 01: multi-stage. The final image holds the wt binaries, the ONNX Runtime library, the
# configuration files the web app reads at start, and nothing else: no shell, no package manager,
# no models. The task's models container copies the models from the models bucket into a shared
# volume before wt-api starts, and wt-api refuses to serve until they load.

FROM rust:1.99-bookworm AS build
WORKDIR /src
ENV SQLX_OFFLINE=true CARGO_TERM_COLOR=never
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p wt-cli -p wt-api \
 && install -D -m 0755 target/release/wt /out/wt \
 && install -D -m 0755 target/release/wt-api /out/wt-api

# ONNX Runtime for wt-ml's load-dynamic build: the release the owner's alpha runs, checked
# against its hash.
FROM debian:bookworm-slim AS ort
ARG ORT_VERSION=1.30.0
ARG ORT_SHA256=a5ed5a3cac51fbb2e90da632ae43d19212faaa20e76484e62bcb7c23ddb3b3fd
ADD --checksum=sha256:${ORT_SHA256} \
    https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}/onnxruntime-linux-x64-${ORT_VERSION}.tgz /ort.tgz
RUN mkdir /ort && tar -xzf /ort.tgz -C /ort --strip-components=2 \
      "onnxruntime-linux-x64-${ORT_VERSION}/lib/libonnxruntime.so.${ORT_VERSION}" \
 && mv "/ort/libonnxruntime.so.${ORT_VERSION}" /ort/libonnxruntime.so

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/wt /out/wt-api /usr/local/bin/
COPY --from=ort /ort/libonnxruntime.so /usr/local/lib/libonnxruntime.so
# Read at start relative to the working directory: WT_CONFIG_DIR defaults to config and
# WT_MODEL_LICENSES to docs/model-licenses.md.
WORKDIR /app
COPY config/ /app/config/
COPY docs/model-licenses.md /app/docs/model-licenses.md
# Evaluation question sets, for wt eval load and wt eval gate in the eval task.
COPY eval/ /app/eval/
ENV WT_ORT_DYLIB=/usr/local/lib/libonnxruntime.so WT_MODEL_DIR=/models
USER nonroot
EXPOSE 8080
ENTRYPOINT []
CMD ["wt-api"]
