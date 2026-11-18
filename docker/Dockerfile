FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p pennyrouter-gateway

FROM debian:bookworm-slim
RUN useradd -m app
USER app
COPY --from=builder /app/target/release/gateway /usr/local/bin/gateway
ENV PORT=8080
EXPOSE 8080
CMD ["/usr/local/bin/gateway"]
