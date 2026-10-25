#!/usr/bin/env bash
# Test the VS Code extension's TextMate grammar, then package the extension
# (editors/vscode) as build/sinter-<version>.vsix. Needs Node.js 22+.
#
# Usage: scripts/vscode.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
die() { warn "$*"; exit 1; }

# Pinned tool versions (npx fetches them on first use).
VSCE="@vscode/vsce@4.0.0"
TMGRAMMAR_TEST="vscode-tmgrammar-test@0.1.3"

have npx || die "npx not found: install Node.js 22 or later"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root/editors/vscode"
version="$(node -p 'require("./package.json").version')"

step "vscode-tmgrammar-test (editors/vscode/test)"
npx --yes "$TMGRAMMAR_TEST" -g syntaxes/sinter.tmLanguage.json 'test/*.test.snt'

mkdir -p "$root/build"
step "vsce package -> build/sinter-${version}.vsix"
npx --yes "$VSCE" package --skip-license --out "$root/build/sinter-${version}.vsix"
