# Builder image: compiles pg_aidb Rust extension.
# Base MUST match Dockerfile.pg to avoid GLIBC mismatch. (HANDOFF.md §6)
FROM pgvector/pgvector:pg17

# Install build toolchain as root (pgvector already installed in base image).
# Rust toolchain installed BEFORE switching to non-root user.
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    pkg-config \
    libssl-dev \
    libclang-dev \
    curl \
    git \
    ca-certificates \
    postgresql-server-dev-17 \
    # dev convenience tools (D3): JSON parsing, process inspection, in-container editing
    jq \
    wget \
    procps \
    vim \
    less \
    tree \
 && rm -rf /var/lib/apt/lists/*

RUN useradd -m -u 1000 dev \
 && chown -R dev:dev \
      /usr/share/postgresql/17/extension \
      /usr/lib/postgresql/17/lib

USER dev
WORKDIR /home/dev

ENV PATH=/home/dev/.cargo/bin:$PATH

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable \
 && . ~/.cargo/env \
 && rustup component add rustfmt clippy

RUN . ~/.cargo/env && cargo install --locked cargo-pgrx --version "=0.18.0"

# Point pgrx at the system PostgreSQL (HANDOFF.md §6 cargo pgrx init pattern)
RUN . ~/.cargo/env && cargo pgrx init --pg17 /usr/bin/pg_config

# Default: keep container alive for interactive / CI use.
# CI override: docker compose run --rm builder bash -c "cd /workspace/extension && ..."
CMD ["sleep", "infinity"]
