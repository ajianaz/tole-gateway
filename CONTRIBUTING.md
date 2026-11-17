# Contributing to tole-gateway

Thank you for contributing! This document is the single entry point for the
workflow; the ecosystem-wide standards it follows live in the team knowledge
rooms (source of truth when this file and reality disagree).

## Workflow

- **Branches**: `develop` is the integration branch (the intended default
  branch); `main` is the release branch (tags only). All work happens as
  branch, then PR, then squash-merge into `develop`. No direct pushes, no force
  pushes to protected branches.
- **Branch names** (CI-enforced): `feat/*`, `fix/*`, `docs/*`, `chore/*`,
  `perf/*`, `security/*`, `refactor/*`, `test/*`, `build/*`, `ci/*`. Note
  `feat/`, not `feature/`.
- **Commits**: Conventional Commits (`feat:`, `fix:`, `docs:`, `chore:`,
  `refactor:`, `test:`). The PR title becomes the squash commit message.
- **Staying current**: when your branch is behind `develop`, rebase onto
  `origin/develop` (do not add a merge commit) and push with
  `--force-with-lease` to your own feature branch only.
- **Hooks**: run `scripts/setup-hooks.sh` once after cloning. It sets
  `core.hooksPath` to `scripts/hooks`, which holds the `commit-msg` hook of the
  team message standard (the pattern is read from `MESSAGE_STANDARD_PATTERN` or
  the local git config key `messagestandard.pattern`; it is not stored here).
  The CI **Message Standard** job applies the same scan to every commit
  message, the PR title and the PR body.
- **Pre-commit**: `cargo fmt --all`, then
  `cargo clippy --workspace --all-targets -- -D warnings`, then
  `cargo test --workspace`, then `cora review --staged`. CI enforces the same
  set (minus the staged review).
- **Code review**: every PR gets a Cora review (below) plus maintainer review
  before merge. CI must be green.

## Issues

- Use the issue templates ([bug report](.github/ISSUE_TEMPLATE/bug_report.yml),
  [feature request](.github/ISSUE_TEMPLATE/feature_request.yml)).
- **Issue-first**: larger changes (a new adapter, a new subsystem, a change to
  the approval or session rules) start as an issue for discussion before any
  PR. One PR = one logical change.
- The roadmap (G1 to G5) lives in [docs/design.md](docs/design.md).

## Pull requests

- One logical change per PR. Large change-sets can time out the Cora Review
  job (30-minute limit): split anything beyond about 15 to 20 files, or about
  600 changed lines of code, into several PRs (a refactor PR and a behavior PR,
  or one PR per subsystem).
- Body sections, in order: **What / Why / Changes / Testing**. A PR without
  them fails the PR Description check.
- Base every PR on `develop`. PRs into `main` are only accepted from
  `develop` or `chore/release-*` (release flow).

## Merge gate: before merging any PR

Never merge on schedule or assumption:

1. All checks pass (`gh pr checks <N>`).
2. The Cora review verdict is **real**: the review result lands as a PR
   comment. It must say "No issues found" or list concrete findings. "Review
   could not complete" or an LLM error is a blocker: re-run until a real
   verdict.
3. Code-scanning alerts on the PR head are investigated one by one.
4. Every finding is validated against the code: real, then fix it with a
   regression test; false positive, then dismiss it with evidence.
5. CI auto-fix loop: at most 3 attempts, then stop and escalate.
6. CI has run on the merge result: the branch is up to date with `develop`
   and green on that head.
7. Squash-merge with an explicit title and message (the squash message is not
   covered by any check, so it is on whoever merges), delete the branch, sync
   `develop`.

## Local review before a PR

The CI Cora Review job can take 12 to 30 minutes on a large diff, so get
findings earlier on your own machine:

1. Install the `cora` binary from its GitHub release (never build it from
   source). For a container, pin the version:
   `CORA_VERSION=<release tag> CORA_INSTALL_DIR=<dir> sh install.sh` from the
   cora-code repository.
2. `scripts/review-local.sh` reviews `git diff origin/develop...HEAD` (use
   `--base REF` for another base), split by crate or directory into chunks of
   at most 600 changed lines (`--max-lines N`), and prints only the findings
   (`--severity` sets the minimum, default `minor`; `--dry-run` prints the
   chunk plan). Exit `0` = no findings, `2` = findings, `1` = error or no LLM
   configured (not a pass).
3. Fix real findings; for a false positive, extend the rule in `.cora.yaml`
   instead of sprinkling inline ignores.
4. State the result in the PR **Testing** section.

Cora is mandatory as reviewer before a PR is opened. `cora review --staged
--no-llm` runs the deterministic checks only; without an LLM key, review
through `cora mcp` (`review_prepare` / `review_finalize`).

The local run does not replace the CI Cora Review job, which remains the merge
gate.

## Cora workflow

1. **Before each commit**: `cora review --staged`. Exit `0` clean; exit `2`
   blocking (major/critical), fix it; exit `1` cora itself errored
   (non-blocking, but it is not a pass).
2. **Manual scan**: `cora scan` after significant or security-adjacent
   changes.
3. **CI**: the `Cora Review` job (`.github/workflows/cora-review.yml`) reviews
   every PR. It needs credentials set by the owner (see
   [docs/repo-settings.md](docs/repo-settings.md)); an empty credential fails
   the job rather than passing silently.

Review behavior is configured in `.cora.yaml`.

## Language standard

All repository artifacts are written in **English**: commit messages, PR
titles and bodies, issues, code comments, and documentation files. Keep chat
discussions in whatever language you prefer; files must be English.

## Releases

Release = version bump + `CHANGELOG.md` + tag on a commit reachable from
`main`. The owner pushes the tag. Versions stay `0.x` until the owner decides
otherwise.

## Design rules

- The gateway is a separate process and a thin client of the `tole serve` API.
  It links no `tole-core` and holds no business logic.
- Approvals go only through tole's single gate. The gateway never approves on
  its own and never offers a Destructive tool for approval.
- Secrets never appear in arguments, logs or tool output.
- Adding code is a last resort: first check whether configuration or an
  existing mechanism solves the problem.

## License

Contributions are licensed to the project under the Apache-2.0 license
(inbound = outbound).
