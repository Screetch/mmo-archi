#!/bin/sh
# Make a git worktree whose build starts warm: its target of cargo is a copy on write of the one of the main checkout,
# or of the worktree of this clone whose commit stands nearest, so cargo builds only what changed.
# Usage: script/worktree.sh <path> <branch> [base, origin/main by default]
# A branch that stands on origin, or in this clone, is checked out and not made again: a local branch that only lags
# behind origin moves up to it, and a local branch with work that origin lacks stays as it is. The base is the start
# of a new branch alone.
set -eu

if [ $# -lt 2 ] || [ $# -gt 3 ]; then
  echo "usage: $0 <path> <branch> [base]" >&2
  exit 2
fi
path=$1
branch=$2
base=${3:-origin/main}
step="read the arguments"
case $path in
  /*) ;;
  *) path=$PWD/$path ;;
esac
trap 'code=$?; if [ "$code" -ne 0 ]; then echo "worktree.sh failed at \"$step\" with exit $code: $path on $branch" >&2; fi' EXIT

common=$(git -C "$(dirname "$0")" rev-parse --path-format=absolute --git-common-dir)
if [ "$(git --git-dir="$common" rev-parse --is-bare-repository)" = true ]; then
  main=$common
else
  main=$(dirname "$common")
fi

step="fetch origin"
# Other workers fetch in the same clone, and two fetches that meet fail with "cannot lock ref". The second try and the
# third wait a little, and the last one prints its own error.
fetched=
for pause in 1 3; do
  if err=$(git -C "$main" fetch -q origin 2>&1); then
    fetched=1
    break
  fi
  echo "The fetch of origin failed, so it runs again in $pause s: $(printf '%s' "$err" | tail -n 1)" >&2
  sleep "$pause"
done
if [ -z "$fetched" ]; then
  git -C "$main" fetch -q origin
fi
# The hooks print only what fails, so that the tail of a command that commits and pushes shows the push. The setting is
# shared by each worktree of the clone, and one that the owner set stays.
git -C "$main" config --get core.hooksPath >/dev/null || git -C "$main" config core.hooksPath script/hooks
step="make the worktree"

# Where the branch stands decides how it opens. Git refuses a branch that another worktree holds, and says where.
if git -C "$main" show-ref --verify -q "refs/heads/$branch"; then
  git -C "$main" worktree add -q "$path" "$branch"
  opened=local
  if [ $# -eq 3 ]; then
    echo "The branch $branch stands already, so the base $base is not used."
  fi
elif git -C "$main" show-ref --verify -q "refs/remotes/origin/$branch"; then
  git -C "$main" worktree add -q -b "$branch" "$path" "origin/$branch"
  opened=remote
  if [ $# -eq 3 ]; then
    echo "The branch $branch stands on origin, so the base $base is not used."
  fi
else
  git -C "$main" worktree add -q -b "$branch" "$path" "$base"
  opened=new
fi

if git -C "$path" show-ref --verify -q "refs/remotes/origin/$branch"; then
  git -C "$path" branch -q --set-upstream-to="origin/$branch" "$branch"
  if [ $opened = local ]; then
    if git -C "$path" merge-base --is-ancestor HEAD "origin/$branch"; then
      git -C "$path" merge -q --ff-only "origin/$branch"
    elif git -C "$path" merge-base --is-ancestor "origin/$branch" HEAD; then
      echo "The local branch $branch holds commits that origin lacks. It stays as it is."
    else
      echo "The local branch $branch and origin/$branch each hold commits that the other lacks. Look at both before you push."
    fi
  fi
fi
if [ $opened != new ]; then
  behind=$(git -C "$path" rev-list --count "HEAD..origin/main" 2>/dev/null || echo 0)
  if [ "$behind" -gt 0 ]; then
    echo "$branch is $behind commits behind origin/main. Rebase it there, or merge origin/main into it, before you build."
  fi
fi

step="copy the build"
# The source of the warm build: of the main checkout and each worktree of this clone that holds a target, the one whose
# commit stands nearest to the commit of the new tree. A target is as fresh as the work done in its tree, and the main
# checkout can stand far behind origin. The main checkout wins a tie.
here=$(git -C "$path" rev-parse HEAD)
src=$main
best=-1
trees=$(git -C "$main" worktree list --porcelain | sed -n 's/^worktree //p')
while IFS= read -r tree; do
  if [ -z "$tree" ] || [ "$tree" = "$path" ] || [ ! -d "$tree/target" ]; then
    continue
  fi
  far=$(git -C "$tree" rev-list --count "$here...HEAD" 2>/dev/null) || continue
  if [ "$best" -lt 0 ] || [ "$far" -lt "$best" ]; then
    src=$tree
    best=$far
  fi
done <<EOF
$trees
EOF
if [ "$src" != "$main" ]; then
  echo "The build is copied from $src, whose commit stands $best commits from $branch."
fi

# A copy on write: clonefile on APFS, a reflink on Btrfs or XFS. Each keeps the timestamps, which cargo compares.
case $(uname) in
  Darwin) clone() { cp -cRp "$1" "$2"; } ;;
  *) clone() { cp -a --reflink=always "$1" "$2"; } ;;
esac
if [ ! -d "$src/target" ]; then
  echo "$src holds no build to copy: $path is ready on $branch, and builds from nothing."
  exit 0
fi
if ! clone "$src/Cargo.toml" "$path/.worktree-probe" 2>/dev/null; then
  echo "This file system has no copy on write: $path builds from nothing."
  exit 0
fi
rm -f "$path/.worktree-probe"

# The output of cargo.
if [ ! -e "$path/target" ]; then
  clone "$src/target" "$path/target"
fi
# Each member of the workspace builds again in a new path, so its incremental state is of no use.
find "$path/target" -type d -name incremental -prune -exec rm -rf {} +

echo "$path is ready on $branch."
