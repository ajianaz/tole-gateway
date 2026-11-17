# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.x (develop) | yes |

## How to Report

Use [GitHub Private Vulnerability Reporting](https://github.com/ajianaz/tole-gateway/security/advisories/new). This keeps the report confidential and visible only to maintainers.

Include as much detail as possible:

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

Never include bot tokens, pairing codes, signing secrets or chat ids in a public issue.

## Scope Notes

- The gateway sits between a chat platform and an agent that runs tools with real-world side effects. Report these as security issues, not bugs: an unauthorized chat reaching the agent, an approval shown or granted without the owner's action, a Destructive tool approvable from chat, a secret appearing in a log, an error or a message, and a forged or replayed webhook delivery being accepted.
