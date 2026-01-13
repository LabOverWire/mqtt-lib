FROM rust:alpine AS builder
RUN apk add --no-cache musl-dev upx
WORKDIR /build
COPY . .
RUN cargo build --release -p mqttv5-cli \
    && strip target/release/mqttv5 \
    && upx --best --lzma target/release/mqttv5

FROM scratch
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /build/target/release/mqttv5 /mqttv5
EXPOSE 1883 8883 8080 8443
ENTRYPOINT ["/mqttv5"]
CMD ["broker", "--help"]
