#!/usr/bin/env bash
set -euo pipefail

elf=$1
raw=$2
llvm_bin=$3

if "$llvm_bin/llvm-objdump" --reloc "$elf" | grep -q 'RELOCATION RECORDS FOR'; then
  echo "unexpected relocation in $elf" >&2
  exit 1
fi

if [[ -n $("$llvm_bin/llvm-nm" --undefined-only "$elf") ]]; then
  echo "unexpected undefined symbol in $elf" >&2
  "$llvm_bin/llvm-nm" --undefined-only "$elf" >&2
  exit 1
fi

if "$llvm_bin/llvm-readobj" --sections "$elf" \
  | grep -Eq 'Name: \.(data|bss|got|plt|tdata|tbss|dynamic|dynsym|init_array|fini_array)'; then
  echo "unexpected runtime section in $elf" >&2
  exit 1
fi

state_offset=$("$llvm_bin/llvm-nm" --numeric-sort "$elf" \
  | awk '$3 == "FSPY_STATE_PTR" { print "0x" $1 }')
if [[ -z $state_offset ]]; then
  echo "missing FSPY_STATE_PTR in $elf" >&2
  exit 1
fi

size=$(wc -c < "$raw" | tr -d ' ')
printf '%s: %s bytes, state pointer patch offset %s\n' "$raw" "$size" "$state_offset"
