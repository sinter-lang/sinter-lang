# Sinter development container.
#
# Adapted from the pico-gs devcontainer, trimmed to what Sinter needs:
#   * Rust toolchain (stable) to build the compiler and run tests.
#   * OSS CAD Suite, which bundles Yosys and Verilator for the conformance
#     oracle (generated Verilog synthesized by Yosys and simulated by Verilator).
#   * Claude Code CLI, installed via Anthropic's native shell installer.
FROM ubuntu:24.04

ARG OSS_CAD_SUITE_VERSION=20260127
ARG OSS_CAD_SUITE_DATE=2026-01-27

ENV DEBIAN_FRONTEND=noninteractive

# Base build + VCS + a few libraries used by the OSS CAD Suite binaries.
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        clang-format \
        cmake \
        curl \
        git \
        gnupg \
        libtcl8.6 \
        locales \
        openssh-client \
        pkg-config \
        python3 \
        python3-pip \
        sudo \
        xz-utils \
    && rm -rf /var/lib/apt/lists/* \
    && locale-gen en_US.UTF-8

# --- OSS CAD Suite (Yosys, Verilator, nextpnr, ...) -------------------------
ENV OSS_CAD_SUITE_PATH=/opt/oss-cad-suite
RUN curl -fsSL \
        "https://github.com/YosysHQ/oss-cad-suite-build/releases/download/${OSS_CAD_SUITE_DATE}/oss-cad-suite-linux-x64-${OSS_CAD_SUITE_VERSION}.tgz" \
        -o /tmp/oss-cad-suite.tgz \
    && tar -xzf /tmp/oss-cad-suite.tgz -C /opt \
    && rm /tmp/oss-cad-suite.tgz
ENV PATH="${OSS_CAD_SUITE_PATH}/bin:${PATH}"

# --- just (task runner) ----------------------------------------------------
# Single entry point for the dev + CI task recipes (see justfile + scripts/).
# Installed system-wide so it is on PATH for every user, including CI-parity
# runs of `just ci` inside the container.
ARG JUST_VERSION=1.54.0
RUN curl --proto '=https' --tlsv1.2 -sSf https://just.systems/install.sh \
        | bash -s -- --tag "${JUST_VERSION}" --to /usr/local/bin \
    && just --version

# --- Editor tooling --------------------------------------------------------
# Node.js packages the VS Code extension (editors/vscode, `vsce`). The
# tree-sitter CLI regenerates the editor grammar's parser
# (editors/tree-sitter-sinter, `just tree-sitter`); keep its version in sync
# with scripts/tree-sitter.sh and the CI workflow.
ARG NODE_VERSION=24.22.1
ARG NODE_SHA256=6c172a9ab6718f7b801fb5a7bd486af2ee5e32758086a04b84fcce2aa77095e6
RUN curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz" \
        -o /tmp/node.tar.xz \
    && echo "${NODE_SHA256}  /tmp/node.tar.xz" | sha256sum -c - \
    && tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 \
    && rm /tmp/node.tar.xz \
    && node --version
ARG TREE_SITTER_VERSION=0.27.1
RUN curl -fsSL "https://github.com/tree-sitter/tree-sitter/releases/download/v${TREE_SITTER_VERSION}/tree-sitter-linux-x64.gz" \
        | gunzip > /usr/local/bin/tree-sitter \
    && chmod +x /usr/local/bin/tree-sitter \
    && tree-sitter --version

# --- mdBook ----------------------------------------------------------------
# Builds the user guide in docs/book (`just book`). Keep the version in sync
# with scripts/book.sh and the CI workflow.
ARG MDBOOK_VERSION=0.5.4
ARG MDBOOK_SHA256=3f28de05dafca9d0f2eab99c662116b0e37b89b1d96a08f8f430b9eeae958cd7
RUN curl -fsSL "https://github.com/rust-lang/mdBook/releases/download/v${MDBOOK_VERSION}/mdbook-v${MDBOOK_VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
        -o /tmp/mdbook.tar.gz \
    && echo "${MDBOOK_SHA256}  /tmp/mdbook.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/mdbook.tar.gz -C /usr/local/bin mdbook \
    && rm /tmp/mdbook.tar.gz \
    && mdbook --version

# --- Non-root user ---------------------------------------------------------
ARG USERNAME=vscode
ARG USER_UID=1000
ARG USER_GID=$USER_UID
RUN if getent passwd $USER_UID >/dev/null; then \
        userdel -r "$(getent passwd $USER_UID | cut -d: -f1)"; \
    fi \
    && groupadd --gid $USER_GID $USERNAME \
    && useradd --uid $USER_UID --gid $USER_GID -m -s /bin/bash $USERNAME \
    && echo "$USERNAME ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/$USERNAME \
    && chmod 0440 /etc/sudoers.d/$USERNAME

USER $USERNAME
ENV HOME=/home/$USERNAME

# --- Rust via rustup (stable; Sinter builds on stable, see rust-toolchain.toml)
ENV RUSTUP_HOME=$HOME/.rustup \
    CARGO_HOME=$HOME/.cargo \
    PATH=$HOME/.cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --component rustfmt --component clippy

# --- Claude Code CLI (Anthropic's native shell installer) ------------------
# Self-contained native binary installed to ~/.local/bin/claude (no Node.js
# runtime). Native installs auto-update in the background, so this seeds the
# image with the latest release at build time.
ENV PATH=$HOME/.local/bin:$PATH
RUN curl -fsSL https://claude.ai/install.sh | bash \
    && claude --version

# Pre-create the cargo registry tree so the devcontainer's named volume
# (mounted at ~/.cargo/registry) inherits vscode ownership on first creation.
# Docker seeds a fresh named volume from the image's mount point; without this
# directory the volume is created root-owned and cargo can't write to it.
RUN mkdir -p $HOME/.cargo/registry/cache \
        $HOME/.cargo/registry/index \
        $HOME/.cargo/registry/src

# Persist tool PATHs for login shells.
RUN echo 'export PATH="$HOME/.local/bin:$HOME/.cargo/bin:/opt/oss-cad-suite/bin:$PATH"' >> $HOME/.bashrc \
    && echo 'export OSS_CAD_SUITE_PATH=/opt/oss-cad-suite' >> $HOME/.bashrc

ENV LANG=en_US.UTF-8
