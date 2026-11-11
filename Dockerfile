FROM rust:1.88-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY docker-entrypoint.sh /app/docker-entrypoint.sh

RUN cargo build --release \
    && install -Dm755 /app/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

FROM golang:1.26-bookworm AS objection-db-builder

WORKDIR /go_aplicacion

COPY go_aplicacion/go.mod go_aplicacion/go.sum ./
RUN go mod download
COPY go_aplicacion/cmd/objeciones ./cmd/objeciones
RUN mkdir -p /out \
    && CGO_ENABLED=0 go build -o /out/he1-objeciones-db ./cmd/objeciones

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/rust_cambia_nombre_planillas_he1 /usr/local/bin/he1-unificar-pdfs
COPY --from=builder /usr/local/bin/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
COPY --from=objection-db-builder /out/he1-objeciones-db /usr/local/bin/he1-objeciones-db

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
