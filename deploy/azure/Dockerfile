# Build context: repository root (see deploy/azure/deploy.sh).
#
# Stage 1 compiles the workspace's `pidge-mcp` binary; stage 2 ships only the
# binary on a distroless base with CA certificates (needed for TLS to
# Microsoft Graph, Entra and Key Vault) and no shell.

FROM rust:1-bookworm AS builder
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p pidge-mcp \
 && strip target/release/pidge-mcp

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /src/target/release/pidge-mcp /usr/local/bin/pidge-mcp
ENV PORT=8080
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/usr/local/bin/pidge-mcp"]
