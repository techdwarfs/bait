FROM rust:1.95-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --bin bait-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /data
COPY --from=builder /app/target/release/bait-server /usr/local/bin/bait-server
ENV BAIT_PORT=7979
EXPOSE 7979
ENTRYPOINT ["/usr/local/bin/bait-server"]
CMD ["/data/repo"]
