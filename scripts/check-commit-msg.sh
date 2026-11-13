#!/usr/bin/env bash
# Validates a commit message file. See docs/github/01.GIT-COMMIT.md.
set -euo pipefail
file=$1
subject=$(head -n1 "$file")

# Git-generated merge and revert subjects are allowed as is
if [[ "$subject" =~ ^(Merge|Revert\ \") ]]; then exit 0; fi

fail() { echo "✖ $1" >&2; echo "  subject: $subject" >&2; echo "  see docs/github/01.GIT-COMMIT.md" >&2; exit 1; }

types='feat|fix|refactor|perf|style|docs|test|build|ci|chore|revert'
[[ "$subject" =~ ^($types)(\([a-z0-9._/-]+\))?!?:\ .+ ]] || fail "subject must be '<type>(<scope>): <subject>' with type one of ${types//|/, }"
desc=${subject#*: }
[[ "$desc" =~ ^[A-Z][a-z] ]] && fail "subject must start lowercase (unless it starts with an acronym or proper noun)"
[[ "$subject" == *. ]] && fail "subject must not end with a period"
(( ${#subject} <= 72 )) || fail "subject is ${#subject} characters, max 72"

second=$(sed -n '2p' "$file")
[[ -z "$second" || "$second" =~ ^# ]] || fail "leave one blank line between subject and body"

if grep -iqE '^co-authored-by:.*(claude|anthropic|gemini|openai|chatgpt|gpt-|copilot|cursor|codex|noreply@google)' "$file"; then
  fail "AI tools must not be listed as co-authors"
fi
exit 0
