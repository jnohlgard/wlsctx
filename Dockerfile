FROM docker.io/library/rust:alpine AS builder
RUN apk add --no-cache musl-dev

WORKDIR /work
COPY Cargo.toml Cargo.lock /work/
COPY src/ /work/src/
RUN --mount=type=cache,dst=/usr/local/cargo/registry,id=cargo-registry \
  TERM=dumb cargo build --release

FROM scratch AS deploy
COPY --from=builder /work/target/release/wlsctx /wlsctx
ENTRYPOINT [ "/wlsctx" ]
CMD []
