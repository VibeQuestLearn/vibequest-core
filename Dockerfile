FROM rust:1.91-slim-bookworm AS builder

WORKDIR /app

RUN apt-get update \
  && apt-get install -y --no-install-recommends pkg-config ca-certificates \
  && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY api ./api
COPY fixtures ./fixtures
COPY scenarios ./scenarios
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim AS runtime

WORKDIR /app

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/vibequest-core /usr/local/bin/vibequest-core

ENV APP_ENV=production
ENV PORT=8080

EXPOSE 8080

CMD ["vibequest-core"]
