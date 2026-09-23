# Build context: repository root (see deploy/azure/deploy.sh).
#
# Stage 1 compiles the workspace's `pidge-mcp` binary; stage 2 ships the
# binary on a slim Python base that carries Microsoft's `markitdown` (used by
# `mail_attachment` to turn PDF/Office attachments into Markdown), running as
# an unprivileged system user.

FROM rust:1-bookworm AS builder
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p pidge-mcp \
 && strip target/release/pidge-mcp

FROM python:3.13-slim-bookworm
RUN pip install --no-cache-dir "markitdown[pdf,docx,xlsx,pptx]==0.1.8" \
 && useradd --system --uid 65532 --create-home pidge
COPY --from=builder /src/target/release/pidge-mcp /usr/local/bin/pidge-mcp
ENV PORT=8080
EXPOSE 8080
USER pidge
ENTRYPOINT ["/usr/local/bin/pidge-mcp"]
