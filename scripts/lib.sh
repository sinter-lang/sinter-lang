#!/usr/bin/env bash
# Shared helpers for the task scripts driven by the justfile.
#
# Every script in this directory sources this file first. Sourcing it gives the
# caller strict shell flags, a working directory pinned to the repository root
# (so the scripts behave identically whether run via `just`, directly, or from
# any sub-directory), and a few tiny logging/probe helpers.
#
# Diagnostics go to stderr so a recipe's stdout carries only the underlying
# tool's output.
set -euo pipefail

# Repository root, resolved from this file's own location (scripts/lib.sh).
# Avoids depending on `git` so the scripts also work in a bare checkout.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# Colourise banners only when stderr is an interactive terminal.
if [ -t 2 ]; then
    _C_STEP=$'\033[1;34m'
    _C_WARN=$'\033[1;33m'
    _C_OFF=$'\033[0m'
else
    _C_STEP=''
    _C_WARN=''
    _C_OFF=''
fi

# step MESSAGE — announce the command a script is about to run.
step() { printf '%s==>%s %s\n' "$_C_STEP" "$_C_OFF" "$*" >&2; }

# info MESSAGE — secondary, indented diagnostic line.
info() { printf '    %s\n' "$*" >&2; }

# warn MESSAGE — non-fatal warning.
warn() { printf '%swarning:%s %s\n' "$_C_WARN" "$_C_OFF" "$*" >&2; }

# have COMMAND — true when COMMAND is on PATH.
have() { command -v "$1" >/dev/null 2>&1; }
