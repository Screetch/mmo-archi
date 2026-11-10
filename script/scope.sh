#!/bin/sh
# Says which jobs of the gates a change needs, from the paths that it changes. The jobs on Linux always hold the whole
# proof of the logic. The job of the crate on macOS and on Windows proves the system alone, and a runner of macOS waits
# in a queue for minutes, so a change runs it only when one of its paths can behave in another way there. The paths
# come one on each line from the standard input, and the answer is the lines key=value that a step of GitHub takes into
# its outputs. A pull request and a push to main give their paths. With --all, which a run by hand and the run of each
# night take, every job runs.
# code: the suite on Linux runs. crate: the crate on Windows and on macOS runs.
# Usage: git diff --name-only <base> HEAD | sh script/scope.sh, or sh script/scope.sh --all
code=false
crate=false
if [ "${1:-}" = --all ]; then
  code=true
  crate=true
else
  while read -r path || [ -n "$path" ]; do
    case $path in
      # A path that no job but the hooks needs: prose, the tools of the repository, the format of the Rust, which the
      # hooks check, the files of the agents, and the workflows that hold their own proof.
      '' | *.md | LICENSE* | .gitignore | rustfmt.toml | .furb/* | .agents/* | .claude/* | script/* \
        | .github/ISSUE_TEMPLATE/* | .github/workflows/publish.yml | .github/workflows/release.yml \
        | .github/workflows/titles.yml | .github/workflows/contract.yml \
        | release-please-config.json | .release-please-manifest.json) ;;
      # Each other path can behave in another way on another system, so code and crate agree today: no path here runs
      # on Linux alone. The crate of the collisions is deterministic on every system that a client runs, so its proof
      # needs each system too.
      *)
        code=true
        crate=true
        ;;
    esac
  done
fi
printf 'code=%s\ncrate=%s\n' "$code" "$crate"
