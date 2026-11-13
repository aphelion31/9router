#!/usr/bin/env bash
# Validates the current branch name. See docs/github/02.GIT-BRANCH.md.
# Allowed: develop, master, release/X.Y.Z, hotfix/X.Y.Z, NRR-<3+ digits>/<type>-<kebab-description>
set -euo pipefail
branch=${1:-$(git rev-parse --abbrev-ref HEAD)}
types='feat|fix|refactor|perf|docs|test|build|ci|chore|core|style|revert'
if [[ "$branch" =~ ^(develop|master|HEAD)$ ]] \
  || [[ "$branch" =~ ^(release|hotfix)/[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || [[ "$branch" =~ ^NRR-[0-9]{3,}/($types)-[a-z0-9]+(-[a-z0-9]+)*$ ]]; then
  exit 0
fi
echo "✖ Branch '$branch' does not match NRR-<number>/<type>-<description>" >&2
echo "  types: ${types//|/, }    example: NRR-001/feat-boilerplate" >&2
echo "  create one with: scripts/new-branch.sh feat-<description>" >&2
exit 1
