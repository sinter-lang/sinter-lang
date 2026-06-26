#!/usr/bin/env bash
# Emit SystemVerilog for every example into build/verilog/<example>/.
#
# Covers both single-file examples (`examples/*.snt`) and multi-file projects
# (`examples/<proj>/main.snt`, built via `sinter build` over the project root).
#
# build/ is git-ignored, so this is a throwaway, dev-container-observable
# snapshot — the emitted Verilog changes as the compiler evolves and is not
# committed. The directory is regenerated wholesale each run. Each example gets
# its own sub-directory (several examples emit a module named `Top`, which would
# otherwise collide).
#
# Usage: scripts/verilog.sh [EMIT]
#   EMIT = verilog (default) | harness | all   (passed to `sinter build --emit`)
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

emit="${1:-verilog}"
out="build/verilog"

step "cargo build --release -p sinter-lang"
cargo build --release -p sinter-lang
bin="target/release/sinter"

step "regenerating $out/ (emit=$emit)"
rm -rf "$out"
mkdir -p "$out"

shopt -s nullglob
ok=0
failed=()

# Roots to build: single-file examples + multi-file project entry points.
roots=(examples/*.snt examples/*/main.snt)
for snt in "${roots[@]}"; do
    case "$snt" in
        */main.snt) name="$(basename "$(dirname "$snt")")" ;; # project: dir name
        *) name="$(basename "$snt" .snt)" ;;
    esac
    dest="$out/$name"
    mkdir -p "$dest"
    if log="$("$bin" build "$snt" --emit "$emit" --out "$dest" 2>&1)"; then
        info "$name -> $dest/"
        ok=$((ok + 1))
    else
        warn "$name failed to build:"
        printf '%s\n' "$log" | sed 's/^/      /' >&2
        failed+=("$name")
    fi
done

if [ "${#failed[@]}" -gt 0 ]; then
    warn "verilog generation failed for: ${failed[*]}"
    exit 1
fi
step "wrote SystemVerilog for $ok example(s) to $out/"
