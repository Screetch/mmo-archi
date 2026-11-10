#!/bin/sh
# Fails on a change that breaks a public item of a crate of the workspace against the last release, unless its title
# declares the break. The public items of each crate are a door of mmo-archi, and a break forces a caller to change
# code, so the title of its pull request holds a ! after the area, as .github/pull_request_template.md says.
# cargo-semver-checks names each break: a change that needs a new major version, whatever the version of the workspace
# says. A break that the base of the pull request holds already, which --since names, was declared by the pull request
# that made it, so it fails nothing. A crate that the tag does not hold has no break. With no tag, no release stands.
# Usage: sh script/contract.sh [--base <tag>] [--since <ref>] [--title <title>]
top=$(git rev-parse --show-toplevel) || exit 1
cd "$top" || exit 1
usage="usage: sh script/contract.sh [--base <tag>] [--since <ref>] [--title <title>]"
tag=
since=
title=
while [ $# -gt 0 ]; do
  [ $# -ge 2 ] || { echo "$usage" >&2; exit 2; }
  case $1 in
    --base) tag=$2 ;;
    --since) since=$2 ;;
    --title) title=$2 ;;
    *) echo "$usage" >&2; exit 2 ;;
  esac
  shift 2
done
[ -n "$tag" ] || tag=$(git tag --list 'v*' --sort=-v:refname | head -n 1)
if [ -z "$tag" ]; then
  echo "No tag v* stands, so no release holds a public item that a change can break."
  exit 0
fi
log=$(mktemp "${TMPDIR:-/tmp}/contract.XXXXXX") || exit 1
trap 'rm -f "$log"' EXIT
# Prints each crate whose public items break against a ref, one on each line, and exits 2 when cargo-semver-checks fails
# for another cause.
breaks() {
  for manifest in crates/*/Cargo.toml; do
    git cat-file -e "$1:$manifest" 2>/dev/null || continue
    name=$(sed -n 's/^name = "\(.*\)"$/\1/p' "$manifest" | head -n 1)
    # The release type minor fails only on a change that needs a new major version, which is a break.
    if cargo semver-checks --package "$name" --baseline-rev "$1" --release-type minor >"$log" 2>&1; then
      continue
    fi
    cat "$log" >&2
    grep -q 'requires new major version' "$log" || return 2
    echo "$name"
  done
}
broken=$(breaks "$tag") || exit 2
if [ -z "$broken" ]; then
  echo "No public item breaks against $tag."
  exit 0
fi
if [ -n "$since" ]; then
  again=$(breaks "$since") || exit 2
  fresh=
  for name in $broken; do
    case " $(echo $again) " in *" $name "*) fresh="$fresh $name" ;; esac
  done
  if [ -z "$fresh" ]; then
    echo "Public items of$(printf ' %s' $broken) break against $tag, and the base $since holds each break already."
    exit 0
  fi
  broken=$fresh
fi
case $title in
  *')!:'*)
    echo "The change breaks public items of$(printf ' %s' $broken) against $tag, and its title declares the break."
    exit 0
    ;;
esac
echo "The change breaks public items of$(printf ' %s' $broken) against $tag: give its title a ! after the area, as" \
  "feat(base)!: ..., and its body a section Breaking changes, as .github/pull_request_template.md says." >&2
exit 1
