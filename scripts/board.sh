#!/usr/bin/env bash
# Emit the board-top Verilog wrapper + pin constraints for each pin-mapped
# example into build/board/<pins-stem>/ (DESIGN §7.6, milestone F5; LPF1).
#
# build/ is git-ignored, so this is a throwaway, dev-container-observable
# snapshot, regenerated wholesale each run. For every `examples/*.pins` file the
# matching `.snt` is the example whose stem the pin-map stem prefixes (so
# `go_board.pins` drives `go_board_counter.snt`), dropping `_`-separated
# suffixes from the pin-map stem until one matches (so `go_board_ecp5.pins`
# does too). A map using ECP5-only settings (`io_type=` / `freq=`) gets an LPF,
# any other a PCF, alongside the inner module's Verilog.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

out="build/board"

step "cargo build --release -p sinter-lang"
cargo build --release -p sinter-lang
bin="target/release/sinter"

step "regenerating $out/"
rm -rf "$out"
mkdir -p "$out"

shopt -s nullglob
ok=0
failed=()
pins_files=(examples/*.pins)
if [ "${#pins_files[@]}" -eq 0 ]; then
    warn "no examples/*.pins files found"
    exit 0
fi

for pins in "${pins_files[@]}"; do
    stem="$(basename "$pins" .pins)"
    # Prefer an exact <prefix>.snt; otherwise accept a single <prefix>*.snt
    # match, trying shorter prefixes until one matches. Refuse to guess when
    # several examples share the prefix (deterministic).
    snt=""
    ambiguous=""
    prefix="$stem"
    while [ -n "$prefix" ]; do
        if [ -f "examples/$prefix.snt" ]; then
            snt="examples/$prefix.snt"
            break
        fi
        cands=(examples/"$prefix"*.snt)
        if [ "${#cands[@]}" -eq 1 ] && [ -f "${cands[0]}" ]; then
            snt="${cands[0]}"
            break
        elif [ "${#cands[@]}" -gt 1 ]; then
            ambiguous="${cands[*]}"
            break
        fi
        case "$prefix" in
            *_*) prefix="${prefix%_*}" ;;
            *) prefix="" ;;
        esac
    done
    if [ -n "$ambiguous" ]; then
        warn "$pins matches multiple sources ($ambiguous); name a <stem>.snt to disambiguate"
        failed+=("$stem")
        continue
    fi
    if [ -z "$snt" ]; then
        warn "no .snt source found for $pins"
        failed+=("$stem")
        continue
    fi

    dest="$out/$stem"
    mkdir -p "$dest"
    constraints=()
    if grep -Eq '^[^#]*(io_type=|freq=)' "$pins"; then
        constraints=(--constraints "$dest/$stem.lpf")
    fi
    if log="$("$bin" build "$snt" --pins "$pins" --out "$dest" "${constraints[@]}" 2>&1)"; then
        info "$(basename "$snt") + $(basename "$pins") -> $dest/"
        ok=$((ok + 1))
    else
        warn "$stem failed to build:"
        printf '%s\n' "$log" | sed 's/^/      /' >&2
        failed+=("$stem")
    fi
done

if [ "${#failed[@]}" -gt 0 ]; then
    warn "board generation failed for: ${failed[*]}"
    exit 1
fi
step "wrote board wrapper + constraints for $ok design(s) to $out/"
