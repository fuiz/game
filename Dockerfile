# Stage 1: Build
FROM rust:1-trixie AS builder

WORKDIR /app

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock ./
COPY game/logic/Cargo.toml game/logic/Cargo.toml
COPY game/server/Cargo.toml game/server/Cargo.toml
COPY game/cloudflare/Cargo.toml game/cloudflare/Cargo.toml
COPY corkboard/logic/Cargo.toml corkboard/logic/Cargo.toml
COPY corkboard/server/Cargo.toml corkboard/server/Cargo.toml
COPY corkboard/cloudflare/Cargo.toml corkboard/cloudflare/Cargo.toml

# Create stub source files so cargo can resolve the workspace and cache dependencies
RUN mkdir -p game/logic/src game/server/src game/cloudflare/src \
             corkboard/logic/src corkboard/server/src corkboard/cloudflare/src && \
    echo '//! stub' > game/logic/src/lib.rs && \
    echo 'fn main() {}' > game/server/src/main.rs && \
    echo '//! stub' > game/cloudflare/src/lib.rs && \
    echo '//! stub' > corkboard/logic/src/lib.rs && \
    echo 'fn main() {}' > corkboard/server/src/main.rs && \
    echo '//! stub' > corkboard/cloudflare/src/lib.rs

# Build dependencies only (cached layer)
RUN cargo build --release --package fuiz-server --package corkboard-server 2>/dev/null || true

# Copy actual source code
COPY . .

# Touch source files to invalidate the stub builds
RUN find game corkboard -name "*.rs" -exec touch {} +

# Build the real binaries
RUN cargo build --release --package fuiz-server --package corkboard-server

# Stage 2: Runtime for fuiz-server
FROM debian:trixie-slim AS fuiz-server

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/fuiz-server /usr/local/bin/fuiz-server

ENV FUIZ_HOSTNAME=0.0.0.0
ENV FUIZ_PORT=8080

EXPOSE 8080

CMD ["fuiz-server"]

# Stage 3: Runtime for corkboard-server
FROM debian:trixie-slim AS corkboard-server

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/corkboard-server /usr/local/bin/corkboard-server

ENV CORKBOARD_HOSTNAME=0.0.0.0
ENV CORKBOARD_PORT=5040

EXPOSE 5040

CMD ["corkboard-server"]
