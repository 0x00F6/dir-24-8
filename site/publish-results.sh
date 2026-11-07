#!/usr/bin/env bash
# Carries the benchmark results of a Pages build over to master
# (.github/workflows/pages.yml, after the benchmarks and site/charts.sh):
# benchmarks/charts/*.png and the generated README blocks (performance
# summary, detailed results, chart list) are copied into a worktree of
# origin/master, committed by github-actions[bot] and pushed. The rest of
# master's README is left as it is. A failure is reported as a warning: the
# site is deployed anyway.
#
# usage: site/publish-results.sh    (from the checkout the workflow measured)
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
branch=${RESULTS_BRANCH:-master}
ref=${GITHUB_REF_NAME:-$(git -C "$root" describe --tags --always)}
tree=${RUNNER_TEMP:-$(mktemp -d)}/results-$branch
export GIT_AUTHOR_NAME="github-actions[bot]" GIT_COMMITTER_NAME="github-actions[bot]"
export GIT_AUTHOR_EMAIL="41898282+github-actions[bot]@users.noreply.github.com"
export GIT_COMMITTER_EMAIL=$GIT_AUTHOR_EMAIL
warn() {
  echo "::warning::benchmark results not committed to $branch: $*"
  exit 0
}

cd "$root"
git fetch --quiet origin "$branch" || warn "cannot fetch $branch"
git worktree remove --force "$tree" 2>/dev/null || rm -rf "$tree"
git worktree prune
git worktree add --quiet --detach "$tree" "origin/$branch" || warn "cannot check out $branch"

mkdir -p "$tree/benchmarks/charts"
cp benchmarks/charts/*.png "$tree/benchmarks/charts/"
python3 site/readme_blocks.py copy README.md "$tree/README.md" \
  bench-compare-lib:summary bench-compare:details benchmark-charts

cd "$tree"
git add README.md benchmarks/charts
if git diff --cached --quiet; then
  echo "benchmark results: nothing changed on $branch"
  exit 0
fi
git commit --quiet -m "Update benchmark results for $ref"
for attempt in 1 2 3; do
  if git push --quiet origin "HEAD:$branch"; then
    echo "benchmark results committed to $branch: $(git rev-parse --short HEAD)"
    exit 0
  fi
  echo "push attempt $attempt failed, rebasing on origin/$branch"
  git pull --quiet --rebase origin "$branch" || warn "rebase on $branch failed"
done
warn "push to $branch failed three times"
