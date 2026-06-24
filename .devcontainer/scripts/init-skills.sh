#!/usr/bin/env bash
# Activate the coding-standard skill submodules declared in .gitmodules.
#
# A committed .gitmodules only declares URLs; the gitlinks that make
# `git submodule update` work must be created with `git submodule add`. This
# script does that idempotently, so it is safe to re-run. It needs SSH access
# to the private `londey`/`systemrs` skill repos; where that is unavailable
# (e.g. an ephemeral sandbox) it skips the affected skill and continues.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# path -> url (kept in sync with .gitmodules).
SKILLS=(
  ".claude/skills/claude-skill-rust|git@github.com:londey/claude-skill-rust.git"
  ".claude/skills/claude-skill-verilog|git@github.com:londey/claude-skill-verilog.git"
  ".claude/skills/ecp5-sv-yosys-verilator|git@github.com:londey/ecp5-sv-yosys-verilator-skill.git"
  ".claude/skills/systemrs-modeling-skill|git@github.com:systemrs/systemrs-modeling-skill.git"
)

for entry in "${SKILLS[@]}"; do
  path="${entry%%|*}"
  url="${entry##*|}"
  if [ -e "$path/.git" ]; then
    echo "skill present: $path"
    continue
  fi
  # Reconcile any stale declaration so `git submodule add` can recreate the
  # gitlink, then add. Tolerate failure (no SSH access in this environment).
  git config -f .gitmodules --remove-section "submodule.$path" 2>/dev/null || true
  if git submodule add -f "$url" "$path" 2>/dev/null; then
    echo "added skill: $path"
  else
    echo "  (skipped $path — needs SSH access to $url)"
  fi
done
