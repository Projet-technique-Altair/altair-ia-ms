# syntax=docker/dockerfile:1.7

FROM rust:1.88-bookworm AS builder
WORKDIR /app

# Build dependency graph first for better cache behavior.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && printf "fn main() {}\n" > src/main.rs
RUN cargo build --release --locked

# Build the real binary.
RUN rm -rf src
COPY src ./src
COPY playbooks ./playbooks
COPY system-prompts ./system-prompts
# The dependency-layer build above creates a placeholder binary. Force the real
# source tree to be newer than that artifact so Cargo cannot reuse the dummy.
RUN touch src/main.rs && cargo build --release --locked && strip /app/target/release/altair-ia-ms

FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app

# Cloud Run convention
ENV PORT=8080
ENV RUST_LOG=info,axum=info,tower_http=info

COPY --from=builder /app/target/release/altair-ia-ms /app/altair-ia-ms
COPY --from=builder /app/playbooks /app/playbooks
COPY --from=builder /app/system-prompts /app/system-prompts

USER nonroot:nonroot
EXPOSE 8080
ENTRYPOINT ["/app/altair-ia-ms"]
