# 0004: Subscriptions run through each vendor's official CLI

- Status: accepted
- Date: 2026-09-23
- Issue: #15

## Context

M2 is done when one subscription login and one API key work, model calls route through them, and usage shows per account. Ryan picked Claude Max, ChatGPT Plus, and Cursor Pro+, with API keys as the fallback (#15). The plan warns that consumer logins in a public open-source app may break vendor terms. This record is desk research as of 2026-09-23, and nothing was run against a real account. Vendor rules changed often in 2026, so recheck the terms before each release.

## Decision

- **wisp never handles consumer credentials.** It has no vendor sign-in of its own. It never reads, stores, or forwards OAuth or session tokens, including `claude setup-token` output, and it never calls a model endpoint with them.
- **Each subscription runs through the vendor's official CLI.** The user installs the CLI and signs in through the vendor's own flow on every machine that runs tasks. `wispd` launches the unmodified CLI once per task and parses its JSON events.
- **API keys use the same adapters.** Keys live in the macOS Keychain and reach the CLI at spawn as `ANTHROPIC_API_KEY`, `CODEX_API_KEY`, or `CURSOR_API_KEY`. wisp's own tool-free calls may use the Anthropic Messages API [17] or the OpenAI Responses API [31]. Cursor has no raw model API [48].
- **Sign-in happens in the editor terminal**, on the machine that needs it, over SSH for a remote host. Status comes from each CLI's own command. Extra accounts with the same vendor get their own config directory (`CLAUDE_CONFIG_DIR` [12], `CODEX_HOME` [24]).
- **The coordinator runs in no-write mode** (see the table), and `wispd` checks `git status` after each coordinator turn.
- **Order.** Claude Code first: one adapter covers a Claude Max login and an Anthropic key, which is enough for M2. Codex second. Cursor last, because its usage data is thinnest and its terms have an open question.

## Provider integration

Versions read on 2026-09-23: Claude Code 2.1.281 [19], Codex CLI 0.156.1 [27], and Cursor CLI 2026.09.23 [42]. "Undocumented" means the field was seen in real output but not in the docs.

| | Claude Max | ChatGPT Plus | Cursor Pro+ |
| --- | --- | --- | --- |
| Binary | `claude` | `codex` | `agent`, with `cursor-agent` as the legacy name [42] |
| Headless run | `claude -p --output-format stream-json --verbose` [10] | `codex exec --json`, with stdin closed or holding the prompt [23][27] | `agent -p --output-format stream-json --trust` [38][41] |
| Events | `system` (`init`), `assistant`, `user`, `rate_limit_event`, `result` [14] | `thread.started`, `turn.started`, `item.started`/`updated`/`completed`, `turn.completed`, `turn.failed`, `error` [27] | `system` (`init`), `user`, `assistant`, `tool_call`, `result`; on failure the stream can end with no terminal event [39] |
| Resume | `--resume <session_id>` [10] | `codex exec resume <thread_id>` [23] | `--resume <chatId>` [38] |
| cwd and worktrees | Process cwd plus `--add-dir`; `-p` never shows the trust dialog [10][13] | `-C <dir>`; must be inside a git repo, and linked worktrees count [23][27] | `--workspace <dir>`; an untrusted folder fails without `--trust` [38][41] |
| Model | `--model` [10] | `-m` [27] | `--model`; list with `agent models` [38] |
| No-write mode | `--tools Read,Glob,Grep --strict-mcp-config --permission-mode dontAsk`; removes tools, no OS sandbox [10][11] | `-s read-only`; Seatbelt enforces it for commands, while MCP servers and web search have separate controls [25] | `--mode ask --sandbox enabled`; staff say modes "were never meant to be isolation" [44] |
| Tool needs approval | Denied, with a `permission_denied` event [11] | exec forces approval policy `never`; a request fails the run [27] | Held back unless `--force` or an allow rule; the docs disagree on whether edits are blocked or only proposed [36][38] |
| Cancel | SIGINT ends the turn; SIGTERM exits 143 and leaves it unfinished [11] | SIGINT interrupts the turn; no SIGTERM handler [27] | Undocumented; kill the process group |
| Signed in | `claude auth status`: JSON, exit 0 or 1 [10] | `codex login status`: text on stderr, exit 0 or 1 [27] | `agent status --format json` [38][40] |
| Plan | `subscriptionType` in `auth status`, undocumented and sometimes stale [18] | `account/read` returns `planType` from `codex app-server` [22] | "Subscription Tier" in `agent about`, undocumented [47] |
| Login command | `claude auth login` [10] | `codex login`, or `--device-auth` on a remote machine [24] | `agent login`, with `NO_OPEN_BROWSER=1` on a remote machine [40] |
| Tokens and cost | `result.usage` and `result.modelUsage[model]`; `total_cost_usd` is a client-side estimate [14][15] | `turn.completed.usage`, cumulative for the thread [27] | `result.usage`: `inputTokens`, `outputTokens`, `cacheReadTokens`, `cacheWriteTokens`; no cost [41][43] |
| Limit windows | `rate_limit_event.rate_limit_info`: `status`, `rateLimitType` (`five_hour`, `seven_day`, ...), `utilization`, `resetsAt` [14] | `account/rateLimits/read` in `codex app-server`: `primary` and `secondary`, each with `usedPercent`, `windowDurationMins`, `resetsAt`; never in exec output [22] | Not in headless output; interactive `/usage` only; no public usage API for individual plans [41][45] |
| API key | `ANTHROPIC_API_KEY` overrides the login in `-p` [12]; direct calls go to `POST https://api.anthropic.com/v1/messages` [17] | `CODEX_API_KEY` for exec [23]; direct calls go to `POST https://api.openai.com/v1/responses` [31] | `CURSOR_API_KEY` [40], which runs the Cursor agent (CLI, SDK, Cloud Agents API) and is not a model API [48] |
| Remote Mac | A locked Keychain over SSH falls back to `~/.claude/.credentials.json` [12] | Stores `$CODEX_HOME/auth.json` by default; no Keychain needed [24][27] | Keychain writes fail over SSH; set `AGENT_CLI_CREDENTIAL_STORE=file` [46] |

## Terms assessment

| Approach | Claude Max | ChatGPT Plus | Cursor Pro+ |
| --- | --- | --- | --- |
| wisp signs in, or uses subscription tokens itself | Prohibited [1][5] | Allowed in practice [22][29][30], but wisp does not do it | Prohibited; risks a ban [35] |
| wisp launches the user's own signed-in official CLI | Allowed, with conditions [1] | Allowed [21][22][23] | Allowed per docs and staff [35][36][37]; the AUP wording is unclear [34] |
| API key in the CLI or the vendor API | Allowed; recommended for third-party tools [3] | Allowed [23] | Agent-level only [35][48] |

- **Anthropic.** Automated access needs an API key unless Anthropic "explicitly permit[s] it" [6]. Third parties may not offer claude.ai login "unless previously approved" [5], and may not collect, store, or intermediate Claude.ai credentials [1]. The permission wisp relies on is on the legal page: nothing prevents "an end user from signing in to the unmodified Claude Code binary with their own Claude subscription" [1]. Its conditions are to leave the binary and its sign-in methods alone, not to pay for or resell usage, and to name Claude Code only in plain text. That text arrived in late August 2026 [2], after a year of reversals. Anthropic blocked spoofed harnesses on Jan 9 [7]. It barred subscription tokens even in the Agent SDK on Feb 19, then removed that line by Apr 13 [2]. It began billing harnesses to extra usage on Apr 4 [9]. It announced a separate `claude -p` credit on May 13 [8] and paused it on Jun 15 [4].
- **OpenAI.** The Terms of Use ban programmatic extraction of output and credential sharing [20]. Even so, OpenAI documents `codex exec` and an app-server for "a deep integration inside your own product" [22][23], and its staff invite ChatGPT sign-in from third-party tools [28][29][30]. I found no 2026 enforcement against third-party clients.
- **Cursor.** Staff say that calling private endpoints with a user's token breaks ToS section 1.5 [33] and "can trigger abuse enforcement, up to and including an account ban". They call the CLI, SDK, and Cloud Agents API "the official and safe path" [35], and the docs pitch the CLI for "scripts and automation workflows" [36]. The open question is the Acceptable Use Policy of Aug 11, 2026. It bans "Accessing the Service through automated or non-human means, whether through a bot, script, or otherwise", with no CLI carve-out [34]. The staff statements came after it.

## Backend interface

A sketch for `wispd` (async omitted):

```rust
trait Backend {
    fn probe(&self, account: &Account) -> Probe;            // installed, version, signed in, auth kind, plan
    fn login_command(&self, account: &Account) -> Command;  // run in the editor terminal
    fn start(&self, account: &Account, task: Task) -> Run;  // cwd, prompt, model, tool policy, resume id
    fn limits(&self, account: &Account) -> Option<Limits>;  // windows, percent used, reset times
}
trait Run {
    fn events(&mut self) -> Events; // Started, Text, ToolCall, FileChange, Usage, Limit, Finished, Failed
    fn cancel(&mut self);
}
```

Gaps it papers over:

- **Usage.** All three report tokens, but only Claude reports cost, as an estimate. Limit percentages come from Claude's stream or a `codex app-server` query; Cursor has none. The UI shows "not reported" rather than zero.
- **Write protection.** Codex has an OS sandbox, Claude removes tools, and Cursor's mode plus sandbox is weakest, hence the `git status` check.
- **Commits.** Codex's `workspace-write` sandbox keeps `.git` read-only, worktrees included [26], so `wispd` commits for Codex workers.
- **Approvals.** All three deny or fail in headless mode, so M2 fixes the policy up front. Prompting the user later needs Claude's `--permission-prompt-tool`, the Codex app-server, or Cursor's ACP.
- **Completion and counters.** Cursor can exit with no terminal event, so completion is the exit code plus the last event. Claude and Codex totals are cumulative per session, so `wispd` stores per-run deltas.
- **Accounts.** Only Codex documents plan detection. Cursor keeps one login in fixed Keychain items [46], so a second Cursor account per machine is unverified.
- **Later options.** ACP is native only in Cursor [50] and carries no usage there [43]. Cursor's Rust-friendly SDK Bridge needs an API key [49].

## Consequences

- Users install and sign into each CLI on each machine; wisp only shows status and opens the login.
- Adapters pin a tested CLI version and run against recorded transcripts in CI.
- Keys in the environment would reach the agent's shell commands, so `wispd` sets `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1` [16] and Codex's `shell_environment_policy.ignore_default_excludes=false` [26]. Cursor has no documented equivalent.
- On a remote Mac, `wispd` runs as a LaunchAgent in the user's login session so the CLIs can reach the Keychain [12][46].
- Routing is per task: wisp picks the account and the CLI makes the model calls.

## Open risks

- **Claude billing.** Anthropic paused but did not drop a separate `claude -p` credit, and it may bill third-party tools to usage credits [3][4]. Plan limits assume "ordinary, individual usage" [1], and parallel subagents will exceed that. Expect to fall back to API keys.
- **Anthropic's product clause.** Running Claude Code "in your products or services" requires the Commercial Terms [1]. It's unclear whether a local open-source launcher counts; a Claude Console account, needed for the API key anyway, would cover it.
- **Cursor.** Get the AUP question answered in writing before release [34]. OpenAI stops supplying models to Cursor on Nov 12, 2026, after SpaceX bought it [32].
- **Schema churn.** Several fields above are undocumented or experimental, including the Codex app-server [22]. Parsers ignore unknown fields.

## Sources

Read on 2026-09-23 unless dated.

Anthropic

1. Claude Code, Legal and compliance: https://code.claude.com/docs/en/legal-and-compliance
2. The same page on the Wayback Machine, 2026-02-19, 2026-04-13, 2026-08-16, and 2026-08-30: https://web.archive.org/web/20260219142355/https://code.claude.com/docs/en/legal-and-compliance, https://web.archive.org/web/20260413021834/https://code.claude.com/docs/en/legal-and-compliance, https://web.archive.org/web/20260816100739/https://code.claude.com/docs/en/legal-and-compliance, https://web.archive.org/web/20260830094710/https://code.claude.com/docs/en/legal-and-compliance
3. Claude Help Center, Log in to your Claude account: https://support.claude.com/en/articles/13189465-log-in-to-your-claude-account
4. Claude Help Center, Use the Claude Agent SDK with your Claude plan (update of 2026-06-15): https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan
5. Agent SDK overview: https://code.claude.com/docs/en/agent-sdk/overview
6. Anthropic Consumer Terms, effective 2025-10-08: https://www.anthropic.com/legal/consumer-terms
7. Thariq Shihipar (Anthropic), 2026-01-09: https://x.com/trq212/status/2009689809875591565
8. Claude Devs, 2026-05-13: https://x.com/ClaudeDevs/status/2054610152817619388
9. TechCrunch, 2026-04-04. This is a secondary source; the customer email is not public: https://techcrunch.com/2026/04/04/anthropic-says-claude-code-subscribers-will-need-to-pay-extra-for-openclaw-support/
10. Claude Code CLI reference: https://code.claude.com/docs/en/cli-reference
11. Claude Code headless mode: https://code.claude.com/docs/en/headless
12. Claude Code authentication: https://code.claude.com/docs/en/authentication
13. Claude Code permissions: https://code.claude.com/docs/en/permissions
14. Agent SDK types 0.3.281 (`SDKRateLimitInfo`, result message): https://cdn.jsdelivr.net/npm/@anthropic-ai/claude-agent-sdk@0.3.281/sdk.d.ts
15. Agent SDK cost tracking: https://code.claude.com/docs/en/agent-sdk/cost-tracking
16. Claude Code environment variables: https://code.claude.com/docs/en/env-vars
17. Claude API, Messages: https://platform.claude.com/docs/en/api/messages
18. anthropics/claude-code issue 94195, which captures `auth status` output: https://github.com/anthropics/claude-code/issues/94195
19. npm registry, `@anthropic-ai/claude-code` (`latest` 2.1.281): https://registry.npmjs.org/@anthropic-ai/claude-code

OpenAI

20. OpenAI Terms of Use, effective 2026-01-01: https://openai.com/policies/terms-of-use/
21. OpenAI Help Center, Using Codex with your ChatGPT plan: https://help.openai.com/en/articles/11369540-using-codex-with-your-chatgpt-plan
22. Codex app-server: https://learn.chatgpt.com/docs/app-server
23. Codex non-interactive mode: https://learn.chatgpt.com/docs/non-interactive-mode
24. Codex authentication: https://learn.chatgpt.com/docs/auth
25. Codex permissions: https://learn.chatgpt.com/docs/permissions
26. Codex approvals and security, and the configuration reference: https://learn.chatgpt.com/docs/agent-approvals-security, https://learn.chatgpt.com/docs/config-file/config-reference
27. Codex CLI 0.156.1 release and source: https://github.com/openai/codex/releases/tag/rust-v0.156.1. Files under `codex-rs/` at that tag: `exec/src/exec_events.rs`, `exec/src/event_processor_with_jsonl_output.rs`, `exec/src/lib.rs`, `cli/src/login.rs`, `config/src/types.rs`, `git-utils/src/info.rs`, `utils/cli/src/shared_options.rs`
28. Codex for Open Source: https://developers.openai.com/community/codex-for-oss
29. Sam Altman, 2026-05-01: https://x.com/sama/status/2050357911915028689
30. Tibo Sottiaux (OpenAI), 2026-05-23: https://x.com/thsottiaux/status/2058071172361998482
31. OpenAI Responses API: https://developers.openai.com/api/reference/resources/responses/methods/create
32. OpenAI, Our decision on Cursor following its acquisition by SpaceX, 2026-08-28: https://openai.com/index/our-decision-on-cursor-following-its-acquisition-by-spacex/

Cursor

33. Cursor Terms of Service, updated 2026-09-03: https://cursor.com/terms-of-service
34. Cursor Acceptable Use Policy, updated 2026-08-11: https://cursor.com/acceptable-use-policy
35. Cursor staff on official clients, 2026-08-10 and 2026-08-16: https://forum.cursor.com/t/167778
36. Cursor headless CLI: https://cursor.com/docs/cli/headless
37. Cursor ACP: https://cursor.com/docs/cli/acp
38. Cursor CLI parameters: https://cursor.com/docs/cli/reference/parameters
39. Cursor CLI output format: https://cursor.com/docs/cli/reference/output-format
40. Cursor CLI authentication: https://cursor.com/docs/cli/reference/authentication
41. Cursor CLI changelog: https://cursor.com/docs/cli/changelog
42. Cursor CLI install script (version 2026.09.23-86fc751): https://cursor.com/install
43. Cursor staff on headless usage fields, 2026-07-01, and on ACP usage, 2026-09-17: https://forum.cursor.com/t/164583, https://forum.cursor.com/t/171923
44. Cursor staff on ask mode and the sandbox, 2026-09-06: https://forum.cursor.com/t/164600
45. Cursor staff on usage APIs for individual plans, 2026-05-19: https://forum.cursor.com/t/160967
46. Cursor staff on the Keychain over SSH, 2026-01-16, and on the file credential store, 2026-08-04: https://forum.cursor.com/t/149045, https://forum.cursor.com/t/167325
47. `agent about` output showing "Subscription Tier Pro+", 2026-09-20: https://forum.cursor.com/t/172448
48. Cursor API overview: https://cursor.com/docs/api
49. Cursor SDK Bridge: https://cursor.com/docs/sdk/bridge

Other

50. Agent Client Protocol, agent list: https://agentclientprotocol.com/overview/agents
