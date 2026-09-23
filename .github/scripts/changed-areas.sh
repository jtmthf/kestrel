#!/usr/bin/env bash
# Classifies the paths a change touched into the areas CI runs. Reads the changed paths from
# stdin, one per line, and writes a `true`/`false` per area to stdout and to GITHUB_OUTPUT. A
# path no area declares turns every area on, because a narrow filter that silently skips a
# check is worse than a slow one.
set -euo pipefail
# Globs are matched, never expanded: without this, a glob word in a loop is a pathname the
# shell tries to expand against the checkout.
set -f

readonly areas="rust images arm64 compose docs"
readonly heavy="rust images arm64 compose"

# Each area declares the paths that make it run. `docs` declares what no heavy area consumes,
# so a documentation-only change is recognised and heavy areas stay off.
glob_for() {
  case "$1" in
  rust)
    echo "crates/** Cargo.toml Cargo.lock rust-toolchain.toml mise.toml openapi/** .kestrel/** .github/**"
    ;;
  images)
    echo "images/** crates/** Cargo.toml Cargo.lock rust-toolchain.toml .dockerignore openapi/** .kestrel/** .github/**"
    ;;
  arm64)
    echo "images/** .dockerignore .github/**"
    ;;
  compose)
    echo "compose.yaml crates/** Cargo.toml Cargo.lock rust-toolchain.toml images/** .dockerignore openapi/** .kestrel/** .github/**"
    ;;
  docs)
    echo "docs/** *.md LICENSE .gitignore .agents/** .claude/** skills-lock.json package.json bun.lock"
    ;;
  esac
}

matches() {
  local path="$1" area="$2" glob
  # shellcheck disable=SC2086 # the glob list is deliberately word-split
  for glob in $(glob_for "$area"); do
    # shellcheck disable=SC2053 # the right-hand side is a glob on purpose
    [[ "$path" == $glob ]] && return 0
  done
  return 1
}

paths=()
while IFS= read -r path; do
  [[ -n "$path" ]] && paths+=("$path")
done

# No readable change, or one path no area declares, is a reason to run everything.
every=true
if [[ ${#paths[@]} -gt 0 ]]; then
  every=false
  for path in "${paths[@]}"; do
    recognised=false
    for area in $areas; do
      if matches "$path" "$area"; then
        recognised=true
        break
      fi
    done
    if ! $recognised; then
      every=true
      break
    fi
  done
fi

for area in $heavy; do
  changed=$every
  if ! $every; then
    for path in "${paths[@]}"; do
      if matches "$path" "$area"; then
        changed=true
        break
      fi
    done
  fi

  echo "$area=$changed"
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    echo "$area=$changed" >>"$GITHUB_OUTPUT"
  fi
done
