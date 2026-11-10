#!/bin/sh
# Fails on each comment line that a change adds to a Rust file past 100 characters: rustfmt sets the width of the code
# at 100 and wraps no comment, so a long comment passes every other guard. It reads the lines that the working tree adds
# to a base, the merge base with origin/main by default, and each line of a Rust file that git does not track yet. A
# line that main holds already is no debt of the change, and goes with the next change of its file. A line inside a
# string or a block comment is no comment line, and a long line of code is the concern of rustfmt.
# Usage: sh script/rustwidth.sh [base]
top=$(git rev-parse --show-toplevel) || exit 1
cd "$top" || exit 1
base=${1:-}
if [ -z "$base" ]; then
  for ref in origin/main main; do
    base=$(git merge-base HEAD "$ref" 2>/dev/null) && break
  done
fi
# A clone with no commit compares each line with the empty tree.
[ -n "$base" ] || base=$(git rev-parse -q --verify HEAD) || base=$(git hash-object -t tree /dev/null)
added=$(mktemp "${TMPDIR:-/tmp}/rustwidth.XXXXXX") || exit 1
trap 'rm -f "$added"' EXIT
{
  git diff -U0 --no-color --no-ext-diff "$base" -- '*.rs' | awk '
    /^\+\+\+ / { file = (substr($0, 1, 6) == "+++ b/") ? substr($0, 7) : ""; next }
    file != "" && /^@@ / {
      n = split(substr($3, 2), part, ",")
      first = part[1] + 0
      count = n > 1 ? part[2] + 0 : 1
      for (line = first; line < first + count; line++) print file "\t" line
    }'
  git ls-files --others --exclude-standard -- '*.rs' | awk '{ print $0 "\t*" }'
} >"$added" || exit 1
# A comment line holds only a comment: the scan follows each string, raw or not, each nested block comment and each
# quote of a character or a lifetime, across the lines of a file. A width counts characters, not bytes.
check=$(cat <<'AWK'
function hashes(n, s) { s = ""; while (n-- > 0) s = s "#"; return s }
function chars(s) { gsub(follow, "", s); return length(s) }
BEGIN { follow = sprintf("[%c-%c]", 128, 191) }
FNR == NR { want[$0] = 1; next }
FNR == 1 { depth = 0; str = 0; raw = -1 }
{
  line = $0; n = length(line); i = 1; lead = 1
  while (i <= n) {
    c = substr(line, i, 1)
    if (depth > 0) {
      if (substr(line, i, 2) == "/*") { depth++; i += 2 } else if (substr(line, i, 2) == "*/") { depth--; i += 2 } else i++
      lead = 0
    } else if (str) {
      if (c == "\\") i += 2; else { if (c == "\"") str = 0; i++ }
      lead = 0
    } else if (raw >= 0) {
      if (c == "\"" && substr(line, i + 1, raw) == hashes(raw)) { i += raw + 1; raw = -1 } else i++
      lead = 0
    } else if (substr(line, i, 2) == "//") {
      if (lead && chars(line) > width && ((FILENAME "\t" FNR) in want || (FILENAME "\t*") in want)) {
        printf "%s:%d: a comment line that this change adds goes past %d characters\n", FILENAME, FNR, width
        found = 1
      }
      break
    } else if (substr(line, i, 2) == "/*") {
      depth = 1; i += 2; lead = 0
    } else if (c == " " || c == "\t") {
      i++
    } else {
      lead = 0
      rest = substr(line, i)
      word = i > 1 && substr(line, i - 1, 1) ~ /[A-Za-z0-9_]/
      if (!word && match(rest, /^b?r#*"/)) { raw = RLENGTH - 2 - (c == "b"); i += RLENGTH }
      else if (!word && match(rest, /^b?"/)) { str = 1; i += RLENGTH }
      else if (c == "'" && substr(line, i + 1, 1) == "\\") { j = index(substr(line, i + 3), "'"); i = j ? i + 3 + j : n + 1 }
      else if (c == "'" && substr(line, i + 2, 1) == "'") i += 3
      else i++
    }
  }
}
END { exit found }
AWK
)
set --
while IFS= read -r file; do
  if [ -f "$file" ]; then set -- "$@" "$file"; fi
done <<EOF
$(cut -f 1 "$added" | sort -u)
EOF
[ $# -gt 0 ] || exit 0
LC_ALL=C awk -v width=100 "$check" "$added" "$@"
