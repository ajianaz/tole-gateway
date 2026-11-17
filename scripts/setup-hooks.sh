#!/usr/bin/env bash
# Activate the repository hooks in scripts/hooks for this clone.
# Run once after cloning: scripts/setup-hooks.sh
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
chmod +x scripts/hooks/*
git config core.hooksPath scripts/hooks
echo "setup-hooks: core.hooksPath set to scripts/hooks"
if [ -z "${MESSAGE_STANDARD_PATTERN:-}" ] && [ -z "$(git config --get messagestandard.pattern || true)" ]; then
  echo "setup-hooks: the commit-msg hook needs the message standard pattern;" >&2
  echo "setup-hooks: export MESSAGE_STANDARD_PATTERN or run" >&2
  echo "setup-hooks:   git config --local messagestandard.pattern '<pattern>'" >&2
  echo "setup-hooks: (ask the maintainers for the value; it is not stored in the repository)" >&2
fi
