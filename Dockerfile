# Force x86_64 — Agave CLI has no aarch64-unknown-linux-gnu builds.
# On Apple Silicon, Docker Desktop runs this via Rosetta emulation.
# (BuildKit emits FromPlatformFlagConstDisallowed for the literal here. The
# constant is intentional: this image MUST be amd64 regardless of host arch.)
FROM --platform=linux/amd64 ubuntu:22.04 AS base

ARG RUST_VERSION=1.86.0
# 4.0.3+ removed the hard io_uring_supported() assertion that crashes
# solana-test-validator on Docker Desktop (where the default seccomp profile
# filters io_uring syscalls). 4.0.2 panics; 4.0.3 falls back gracefully.
ARG AGAVE_VERSION=v4.0.3
ARG NODE_MAJOR=22
# Must match package.json "packageManager".
ARG PNPM_VERSION=10.6.1
# Must match Anchor.toml [toolchain] anchor_version.
ARG ANCHOR_VERSION=0.31.1
ARG PLATFORM_TOOLS_VERSION=v1.54
# Which program to pre-warm. Override with --build-arg PROGRAM=<name> once the
# first program lands under programs/.
ARG PROGRAM=bye_machine

ENV DEBIAN_FRONTEND=noninteractive
ENV LANG=C.UTF-8 LC_ALL=C.UTF-8

RUN apt-get update && apt-get install -y --no-install-recommends \
        curl \
        ca-certificates \
        build-essential \
        pkg-config \
        libssl-dev \
        libudev-dev \
        git \
        gnupg \
        bzip2 \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

# ── Rust toolchain ───────────────────────────────────────────────────────────
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain "${RUST_VERSION}" --profile minimal --no-modify-path
ENV PATH="/root/.cargo/bin:${PATH}"
ENV CARGO_HOME=/root/.cargo
ENV RUSTUP_HOME=/root/.rustup

# ── Solana / Agave CLI ───────────────────────────────────────────────────────
RUN sh -c "$(curl -sSfL https://release.anza.xyz/${AGAVE_VERSION}/install)"
ENV PATH="/root/.local/share/solana/install/active_release/bin:${PATH}"

# Pre-fetch platform-tools to avoid the first-build download on cold caches.
# `cargo-build-sbf --version` triggers the toolchain fetch without compiling.
RUN cargo-build-sbf --tools-version "${PLATFORM_TOOLS_VERSION}" --version || true

# ── Node.js (NodeSource binary distribution) ────────────────────────────────
RUN curl -fsSL "https://deb.nodesource.com/setup_${NODE_MAJOR}.x" | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g "pnpm@${PNPM_VERSION}" --no-audit --no-fund

# ── Anchor CLI ───────────────────────────────────────────────────────────────
# Installed from crates.io rather than the npm wrapper so the version matches
# Anchor.toml exactly. BuildKit cache mounts on the cargo registry/git caches
# keep image rebuilds fast even when this layer is re-executed.
RUN --mount=type=cache,target=/root/.cargo/registry,sharing=locked \
    --mount=type=cache,target=/root/.cargo/git,sharing=locked \
    cargo install --version "${ANCHOR_VERSION}" anchor-cli --locked \
    || cargo install --version "${ANCHOR_VERSION}" anchor-cli

# ── Smoke-check pinned versions at image build time ──────────────────────────
RUN set -eux; \
    rustc --version; \
    cargo --version; \
    solana --version; \
    node --version; \
    pnpm --version; \
    anchor --version

WORKDIR /work

# ── Pre-warm program dependencies ────────────────────────────────────────────
# Bake compiled dep artifacts into /work/target so the named `target` volume is
# seeded with them on first `docker compose run --rm build`.
#
# NOTE: this stage activates once a program exists under programs/${PROGRAM} with
# a Cargo.toml. Until the first program lands (the on-chain design is a later
# phase) the pre-warm is a no-op skeleton.
#
# Strategy:
#   1. Copy the workspace manifests and programs/ (see the COPY note below).
#   2. Stub the program's src/ with a placeholder lib.rs.
#   3. Run `cargo build-sbf` with the production flags — compiling every direct
#      and transitive dep listed in Cargo.lock.
#   4. Wipe the stub src so the runtime bind-mount of host source is clean.
COPY Cargo.toml Cargo.lock* ./
# NB: single-stage build, so PROGRAM / PLATFORM_TOOLS_VERSION declared above are still
# in scope here. Do NOT re-declare them — a bare `ARG X` drops the default and resolves
# to an empty string unless --build-arg supplies one.
#
# Copies the whole programs/ tree rather than just the member manifest. The
# manifest-only optional-COPY idiom (`programs/${PROGRAM}/Cargo.tom[l]`) FAILS under
# BuildKit when the glob matches nothing — it aborts with
# `lstat /programs/${PROGRAM}: no such file or directory` — which is exactly the
# scaffold state, where no program exists yet. The cost is that this layer also
# invalidates on source edits, not just manifest changes; that only re-runs the dep
# pre-warm on image rebuilds, since `docker compose run build` compiles against the
# bind-mounted source and the named `target` volume.
COPY programs ./programs
RUN if [ -f "programs/${PROGRAM}/Cargo.toml" ]; then \
        rm -rf programs/${PROGRAM}/src \
        && mkdir -p programs/${PROGRAM}/src \
        && echo "pub fn _stub() {}" > programs/${PROGRAM}/src/lib.rs; \
    fi

RUN --mount=type=cache,target=/root/.cargo/registry,sharing=locked \
    --mount=type=cache,target=/root/.cargo/git,sharing=locked \
    if [ -f "programs/${PROGRAM}/Cargo.toml" ]; then \
        cargo build-sbf --features dev,testing --arch v3 --tools-version "${PLATFORM_TOOLS_VERSION}" \
            --manifest-path programs/${PROGRAM}/Cargo.toml; \
    fi

# Strip stub-program artifacts so the runtime compile sees only the dep cache.
RUN if [ -d "programs/${PROGRAM}/src" ]; then \
        rm -rf programs/${PROGRAM}/src \
        && find target -name "${PROGRAM}*" -prune -exec rm -rf {} + \
        && find target -name "lib${PROGRAM}*" -prune -exec rm -rf {} +; \
    fi

# Default to an interactive shell; service `command:` overrides this.
CMD ["/bin/bash"]
