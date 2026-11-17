<!--
PR title must follow Conventional Commits: it becomes the squash commit message.
Format: type(scope): short description
Examples: fix(approvals): treat a timeout as a denial / feat(telegram): add the pairing flow / docs(design): record the session rollover rule
-->

## What
<!-- One or two sentences describing the change. -->

## Why
<!-- The problem you are solving. Link the issue if there is one (e.g. "Closes #42"). -->

## Changes
<!-- The change by file or area. Note anything non-obvious about the approach. -->

## Testing

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `scripts/review-local.sh` run: <!-- "no findings", or the findings and how each was handled -->
- [ ] Manual smoke test of the affected behavior <!-- describe what you tested -->

## Checklist

- [ ] Branch name follows the convention (`feat/`, `fix/`, `docs/`, `chore/`, `perf/`, `security/`, `refactor/`, `test/`, `build/`, `ci/`)
- [ ] Branch is based on `develop`
- [ ] Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)
- [ ] No secrets or credentials committed
- [ ] README status and `docs/design.md` roadmap updated if this change makes them stale
- [ ] One logical change per PR
