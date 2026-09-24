# wisp

An open-source macOS app that reproduces the Cursor Projects workflow on machines you own. One coordinator chat plans the work and spawns subagents that share project context. Model calls go through your own AI subscriptions, managed inside the app, with raw API keys as a fallback.

wisp has two parts:

- An editor app built on a stripped-down fork of VS Code (Code - OSS)
- `projectd`, a Rust daemon that runs the coordinator, triggers, and project state on the host

Status: early development. See [docs/PLAN.md](docs/PLAN.md) for the plan and milestones.

## License

[Apache-2.0](LICENSE)
