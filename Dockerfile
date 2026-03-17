FROM rust:1.94-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev git bash \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependency builds: copy manifests first
COPY Cargo.toml Cargo.lock* ./
COPY crates/symphony-core/Cargo.toml crates/symphony-core/Cargo.toml
COPY crates/symphony-workflow/Cargo.toml crates/symphony-workflow/Cargo.toml
COPY crates/symphony-tracker/Cargo.toml crates/symphony-tracker/Cargo.toml
COPY crates/symphony-workspace/Cargo.toml crates/symphony-workspace/Cargo.toml
COPY crates/symphony-agent/Cargo.toml crates/symphony-agent/Cargo.toml
COPY crates/symphony-orchestrator/Cargo.toml crates/symphony-orchestrator/Cargo.toml
COPY crates/symphony-server/Cargo.toml crates/symphony-server/Cargo.toml
COPY crates/symphony-logging/Cargo.toml crates/symphony-logging/Cargo.toml
COPY crates/symphony-cli/Cargo.toml crates/symphony-cli/Cargo.toml

# Create stub lib/main files so cargo can resolve the workspace
RUN for crate in symphony-core symphony-workflow symphony-tracker symphony-workspace \
    symphony-agent symphony-orchestrator symphony-server symphony-logging; do \
    mkdir -p "crates/$crate/src" && echo "" > "crates/$crate/src/lib.rs"; \
    done && \
    mkdir -p crates/symphony-cli/src && echo "fn main() {}" > crates/symphony-cli/src/main.rs

RUN cargo build --release 2>/dev/null || true

# Copy real source and build
COPY crates/ crates/
RUN cargo build --release

# -------------------------------------------------------------------
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates bash git openssh-client curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash symphony

COPY --from=builder /app/target/release/symphony /usr/local/bin/symphony

USER symphony
WORKDIR /home/symphony

# Default workspace root inside the container
ENV SYMPHONY_WORKSPACE_ROOT=/home/symphony/workspaces

ENTRYPOINT ["symphony"]
CMD ["./WORKFLOW.md"]
