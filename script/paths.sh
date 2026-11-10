#!/bin/sh
# Fails on each path of this repository that a script or a workflow names and that no file or folder holds. A move of a
# file leaves its old path in a script that no check of that change runs, and the script fails later. It reads each file
# of script/ and of .github/workflows/, and checks each path whose first part is a folder of the repository. A path that
# git ignores, as one that a build makes, stands. A path with a variable or a pattern in it is no whole path, so it is
# not checked. A path after $root/ or ${root}/, a name that a script can give to the root of the repository, is checked.
# Usage: sh script/paths.sh
top=$(git rev-parse --show-toplevel) || exit 1
cd "$top" || exit 1
work=$(mktemp -d "${TMPDIR:-/tmp}/paths.XXXXXX") || exit 1
trap 'rm -rf "$work"' EXIT
# The files that git tracks or would track and that stand, so a file that git does not hold yet counts too.
git ls-files --cached --others --exclude-standard | while IFS= read -r file; do
  if [ -e "$file" ]; then printf '%s\n' "$file"; fi
done >"$work/files"
set --
while IFS= read -r file; do
  case $file in script/* | .github/workflows/*) set -- "$@" "$file" ;; esac
done <"$work/files"
[ $# -gt 0 ] || exit 0
named=$(cat <<'AWK'
FNR == NR {
  files[$0] = 1
  for (at = length($0); at > 1; at--) if (substr($0, at, 1) == "/") folders[substr($0, 1, at - 1)] = 1
  if ((at = index($0, "/")) > 1) { top = substr($0, 1, at - 1); gsub(/[.]/, "[.]", top); tops[top] = 1 }
  next
}
!ready {
  for (top in tops) alt = alt (alt == "" ? "" : "|") top
  before = "([^A-Za-z0-9_./@$}-]|[$](root|ROOT)/|[$][{](root|ROOT)[}]/)"
  pattern = "(^|" before ")(" alt ")/[-A-Za-z0-9_./@+~%*?$#!^]*"
  ready = 1
}
{
  rest = $0
  while (match(rest, pattern)) {
    path = substr(rest, RSTART, RLENGTH)
    rest = substr(rest, RSTART + RLENGTH)
    sub("^" before, "", path)
    sub(/[.\/]+$/, "", path)
    if (path !~ /[*$?%]/ && !(path in files) && !(path in folders)) print FILENAME "\t" FNR "\t" path
  }
}
AWK
)
LC_ALL=C awk "$named" "$work/files" "$@" >"$work/named" || exit 1
tab=$(printf '\t')
found=0
while IFS=$tab read -r file line path; do
  if git check-ignore -q -- "$path" || git check-ignore -q -- "$path/"; then continue; fi
  echo "$file:$line: $path is no file or folder of git"
  found=1
done <"$work/named"
exit "$found"
