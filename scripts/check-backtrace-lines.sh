#!/usr/bin/env bash
# Checks that a release binary can name the file and line of its own code in a
# panic report: the line tables are present and a known function resolves to
# its source file and a line. A symbolizer reads the same sections, so what
# fails here would fail in a report (AGENTS.md, panic report).
#
# Usage: scripts/check-backtrace-lines.sh <binary>
#
# Needs the llvm-tools rustup component, which reads any target's ELF whatever
# the host, so the arm64 image is checked as well as the amd64 one.
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <binary>" >&2
    exit 2
fi
binary=$1

host=$(rustc -vV | sed -n 's/^host: //p')
tools="$(rustc --print sysroot)/lib/rustlib/$host/bin"
for tool in llvm-nm llvm-objdump; do
    if [ ! -x "$tools/$tool" ]; then
        echo "$tool not found in $tools; run: rustup component add llvm-tools" >&2
        exit 2
    fi
done

# A function in coffer's own code that no inlining removes: the panic hook
# installer is public and the binary calls it first.
symbols=$("$tools/llvm-nm" --defined-only "$binary")
function_symbol=$(printf '%s\n' "$symbols" \
    | awk '$3 ~ /^_RNv.*13coffer_server3cli12panic_report18install_panic_hook$/ && !found { print $3; found = 1 }')
if [ -z "$function_symbol" ]; then
    echo "FAIL: the binary has no symbol for coffer_server::cli::panic_report::install_panic_hook." >&2
    echo "A stripped binary (strip in [profile.release]) loses function names in a panic report." >&2
    exit 1
fi

# Whole-line match on the disassembly's source comments: "; <path>:<line>".
listing=$("$tools/llvm-objdump" --disassemble --line-numbers --no-show-raw-insn \
    --disassemble-symbols="$function_symbol" "$binary")
location=$(printf '%s\n' "$listing" \
    | awk '/^; .*crates\/server\/src\/cli\/panic_report\.rs:[1-9][0-9]*$/ && !found { print $2; found = 1 }')
if [ -z "$location" ]; then
    echo "FAIL: $function_symbol does not resolve to crates/server/src/cli/panic_report.rs and a line." >&2
    echo "The line tables are missing or unreadable: check debug and strip in [profile.release]" >&2
    echo "(Cargo.toml) and the linker flags in the Dockerfile." >&2
    exit 1
fi
echo "ok: install_panic_hook resolves to $location"
