# AGENTS.md

Guidance for coding agents working in this repository. Humans: see
[CONTRIBUTING.md](CONTRIBUTING.md); everything there applies to agents too.
Follows the ecosystem AGENTS.md standard: repo layout, branch and merge
discipline, pre-commit checks, tooling, cross-session continuity, infra facts.

## 1. Repo layout

- `crates/tole-gateway`: the gateway binary (one crate for now; the layering
  and module plan live in `docs/design.md`).
- `docs/`: `design.md` (decisions and roadmap) and `repo-settings.md` (owner
  checklist for repository settings).
- `scripts/`: `review-local.sh` (local Cora review of the branch before a PR),
  `setup-hooks.sh` and `hooks/` (the commit-msg hook).
- `.github/`: workflows, issue and PR templates, dependabot.
- `.cora.yaml`: Cora review configuration; known false-positive categories are
  encoded there as rules. Extend the rule instead of sprinkling inline ignores.

## 2. Branch and merge discipline

- `develop` = integration (the intended default branch), `main` = release
  (tags only). Work happens as branch, PR, squash-merge into `develop`. No
  direct pushes, no force pushes to protected branches. Branch names
  (CI-enforced): `feat/*`, `fix/*`, `docs/*`, `chore/*`, `perf/*`,
  `security/*`, `refactor/*`, `test/*`, `build/*`, `ci/*`. Note `feat/`, not
  `feature/`.
- When behind `develop`, rebase onto `origin/develop`; no merge commits.
- **Language:** every repository artifact (commits, PRs, issues, code
  comments, docs) is written in English.
- **Commits:** Conventional Commits; the PR title becomes the squash message.
  Run `scripts/setup-hooks.sh` once per clone; the CI **Message Standard** job
  scans commit messages, the PR title and the PR body.
- **PR body:** sections in order: **What / Why / Changes / Testing** (enforced
  by the PR Description check).
- **Issues:** larger changes are issue-first; use the issue templates.
- **One PR = one logical change.** Split large change-sets into PRs of about
  15 to 20 files; the Cora Review job has a 30-minute limit.
- **Merge gate, never merge on assumption:** CI green AND a *real* Cora
  verdict in the PR comments ("No issues found" or concrete findings; "Review
  could not complete" is a blocker), code-scanning alerts investigated (real:
  fix with a regression test; false positive: dismiss with evidence), CI
  auto-fix max 3 attempts then escalate. Squash-merge with an explicit title
  and message.
- **Release:** version bump + `CHANGELOG.md` + tag on a commit reachable from
  `main`; the owner pushes the tag. Versions stay `0.x`.

## 3. Mandatory pre-commit checks

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cora review --staged          # exit 0 clean, 2 blocking (fix), 1 cora error (not a pass)
```

CI enforces the same set. Install `cora` from its GitHub release binary, never
build it. Without an LLM key, `cora review --staged --no-llm` runs the
deterministic checks only; the agent-as-reviewer path is `cora mcp`.

**Before opening a PR** (mandatory) run Cora on the whole branch, not just the
staged diff, and fix real findings first:

```bash
scripts/review-local.sh        # git diff origin/develop...HEAD, split by crate/dir, findings only
```

State the result in the PR **Testing** section.

## 4. Tooling: cora first

- `cora index --stats`: confirm the repo is indexed before relying on search.
- `cora brain "<topic>" --json`: semantic recall of existing patterns before
  writing code; prefer this over blind grep.
- `cora callers <sym>` / `cora impact <sym>` / `cora affected <files>`: run
  them before moving, renaming or removing a public item, changing a config or
  state file format, or touching the allowlist, approval or secret-handling
  paths. Put what they showed in the PR description. On Rust these commands
  are noisy (calls resolve by bare name, macro bodies are not parsed): treat
  the output as a pointer and verify every caller and test with grep.
- `cora review --staged` before every commit, `scripts/review-local.sh` before
  every PR, `cora scan` after significant or security-adjacent changes.
- `cora dead-code` after removals; `cora debt` for trend checks.

## 5. Session start (continuity)

- New session or round: read the team knowledge rooms before any work, then
  recall this repo's context.
- Save decisions and important outcomes back to the responsible room during or
  at the end of the session.
- Handing over mid-task: leave a `[HANDOFF]` entry (current state, next step,
  blockers).
- Repo files are canonical over mirrors.

## 6. Design rules: do not bend

- The gateway is a separate process and a thin client of the `tole serve`
  API. It links no `tole-core` and holds no business logic.
- Approvals go only through tole's single gate. The gateway never approves on
  its own, never caches an approval, and an unanswered approval is a denial.
  Destructive tools are never approvable from chat (tole enforces it; the
  gateway must not offer a way around it).
- Only authorized chats reach tole: the config allowlist, or the one-time
  pairing code. No other path.
- Secrets (bot token, signing secret, API keys, pairing code) never appear in
  arguments, logs, error messages or tool output.
- The channel seam is extracted only once two adapters exist.
- Adding code is a last resort: first check whether configuration or an
  existing mechanism solves the problem.

## 7. Docs sync

The README status line and the `docs/design.md` roadmap must be updated **in
the same PR** that makes them stale.

## 8. Infra facts (verified only, with dates)

None yet. Add an entry only after verifying it, with the date.
