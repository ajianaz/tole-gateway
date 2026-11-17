# Repository settings (owner checklist)

These settings can only be changed by the repository owner. The workflows in
`.github/workflows/` assume them. Never write a secret value into the
repository, an issue or a PR.

## Branches

- [ ] Change the default branch to `develop` (Settings, General, Default branch).
- [ ] Delete the `master` branch after the default is changed.
- [ ] `main` is the release branch: tags only, no direct pushes.

## Ruleset for `develop` and `main`

- [ ] Require a pull request before merging (no direct pushes).
- [ ] Block force pushes and branch deletion.
- [ ] Require branches to be up to date before merging (strict status checks).
- [ ] Required status checks, by exact job name:
  - `Check`
  - `Format`
  - `Clippy`
  - `Test`
  - `Build`
  - `Cargo Audit`
  - `Cora Review`
  - `Branch Naming`
  - `Conventional Commits`
  - `PR Description`
  - `Message Standard`
  - `Source Branch` (only meaningful on PRs into `main`; add it to the `main` ruleset only)
  - Optional: `Trivy FS Scan`

## Merging

- [ ] Allow squash merging only (disable merge commits and rebase merging).
- [ ] Default squash commit message: pull request title and description.
- [ ] Automatically delete head branches after merge.
- [ ] Enable Dependabot alerts and security updates.
- [ ] Enable private vulnerability reporting (referenced by `SECURITY.md`).
- [ ] Enable code scanning alerts if available.

## Repository variables and secrets

Settings, Secrets and variables, Actions.

| Name | Kind | Used by | Notes |
|------|------|---------|-------|
| `MESSAGE_STANDARD_PATTERN` | variable | Message Standard job, commit-msg hook | An extended regular expression (`grep -E`, case-insensitive). The job fails when it is empty or invalid, by design: a skipped check must never look like a pass. The value is not stored in the repository. |
| `CORA_API_KEY` | secret | Cora Review job | API key of the review provider. |
| `CORA_BASE_URL` | secret or variable | Cora Review job | Prefer a variable so the endpoint shows in the log. |
| `CORA_MODEL` | secret or variable | Cora Review job | Prefer a variable so the model shows in the log. |

The Cora Review job fails (it does not skip) when any of the three `CORA_*`
values is empty. The same applies to `MESSAGE_STANDARD_PATTERN`.

Local use of the commit-msg hook: `scripts/setup-hooks.sh`, then export
`MESSAGE_STANDARD_PATTERN` or run
`git config --local messagestandard.pattern '<pattern>'`.
