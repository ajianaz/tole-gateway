# tole-gateway

[![CI](https://github.com/ajianaz/tole-gateway/actions/workflows/ci.yml/badge.svg)](https://github.com/ajianaz/tole-gateway/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

A small chat gateway for [`tole serve`](https://github.com/codecoradev/tole).
A separate process and a thin client of the tole serve API: it links no
`tole-core` and holds no business logic. The first channel is Telegram (one
bot, long polling, no inbound port); Discord follows. Single user first.

**Status:** G1, repository bootstrap. The binary only answers `--version` and
`--help`; there is no tole client code yet. See the roadmap in
[docs/design.md](docs/design.md).

## Build

```bash
cargo build --workspace --release
./target/release/tole-gateway --version
```

## Documentation

- [docs/design.md](docs/design.md): decisions, module plan and roadmap.
- [CONTRIBUTING.md](CONTRIBUTING.md): workflow, branches, PRs, review gate.
- [docs/repo-settings.md](docs/repo-settings.md): owner checklist for repository settings.
- [SECURITY.md](SECURITY.md): how to report a vulnerability.

## License

Apache-2.0, see [LICENSE](LICENSE).
