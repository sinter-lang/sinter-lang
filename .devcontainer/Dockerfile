# Sinter development container.
#
# Adapted from the pico-gs devcontainer, trimmed to what Sinter needs:
#   * Rust toolchain (stable) to build the compiler and run tests.
#   * OSS CAD Suite, which bundles Yosys and Verilator for the conformance
#     oracle (generated Verilog synthesized by Yosys and simulated by Verilator).
#   * Node.js for the Claude Code CLI.
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

# --- Node.js 20.x LTS (for the Claude Code CLI) ----------------------------
RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# --- just (task runner) ----------------------------------------------------
# Single entry point for the dev + CI task recipes (see justfile + scripts/).
# Installed system-wide so it is on PATH for every user, including CI-parity
# runs of `just ci` inside the container.
ARG JUST_VERSION=1.54.0
RUN curl --proto '=https' --tlsv1.2 -sSf https://just.systems/install.sh \
        | bash -s -- --tag "${JUST_VERSION}" --to /usr/local/bin \
    && just --version

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

# Pre-create the cargo registry tree so the devcontainer's named volume
# (mounted at ~/.cargo/registry) inherits vscode ownership on first creation.
# Docker seeds a fresh named volume from the image's mount point; without this
# directory the volume is created root-owned and cargo can't write to it.
RUN mkdir -p $HOME/.cargo/registry/cache \
        $HOME/.cargo/registry/index \
        $HOME/.cargo/registry/src

# Persist tool PATHs for login shells.
RUN echo 'export PATH="$HOME/.cargo/bin:/opt/oss-cad-suite/bin:$PATH"' >> $HOME/.bashrc \
    && echo 'export OSS_CAD_SUITE_PATH=/opt/oss-cad-suite' >> $HOME/.bashrc

ENV LANG=en_US.UTF-8
