# syntax=docker/dockerfile:1.7
# Spec 01: multi-stage. The final image holds the wt binaries and nothing else: no shell, no
# package manager, no models. Models load from the models bucket at start (M5), and the ONNX
# Runtime library joins this image, located through ORT_DYLIB_PATH, when wt-ml needs it.

FROM rust:1.99-bookworm AS build
WORKDIR /src
ENV SQLX_OFFLINE=true CARGO_TERM_COLOR=never
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p wt-cli -p wt-api \
 && install -D -m 0755 target/release/wt /out/wt \
 && install -D -m 0755 target/release/wt-api /out/wt-api

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/wt /out/wt-api /usr/local/bin/
USER nonroot
EXPOSE 8080
ENTRYPOINT []
CMD ["wt-api"]
