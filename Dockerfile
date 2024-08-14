FROM rust:alpine AS builder

WORKDIR /app

RUN apk add --no-cache pkgconfig openssl openssl-dev musl-dev

COPY . .

RUN cargo build --bin cloudflare-tunnels-operator --release --locked

FROM scratch AS runtime

WORKDIR /app

COPY --from=builder --chown=nonroot:nonroot /app/target/release/cloudflare-tunnels-operator .

CMD ["/app/cloudflare-tunnels-operator"]
