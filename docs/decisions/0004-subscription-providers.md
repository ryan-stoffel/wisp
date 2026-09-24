# 0004: Subscriptions run through each vendor's official CLI

- Status: accepted
- Date: 2026-09-23
- Issue: #15

## Context

M2 is done when one subscription login and one API key work, model calls route through them, and usage shows per account. Ryan picked Claude Max, ChatGPT Plus, and Cursor Pro+, with API keys as the fallback (#15). The plan warns that consumer logins in a third-party app may break vendor terms. This is desk research as of 2026-09-23, with no runs against real accounts. Recheck the terms before each release.

## Decision

- **wisp never handles consumer credentials.** It has no vendor sign-in of its own. It never reads, stores, or forwards OAuth or session tokens, including `claude setup-token` output, and never calls a model endpoint with them.
- **Subscriptions run through the vendor's official CLI.** The user installs it and signs in through the vendor's own flow in the editor terminal, on every machine that runs tasks. `wispd` runs the unmodified CLI once per task and parses its JSON events.
- **API keys use the same adapters** and are kept in the macOS Keychain. wisp's own tool-free calls may use the vendor API directly.
- **The coordinator runs only on Claude Code or Codex**, in no-write mode, with `wispd`'s tools served over MCP. If `git status` changes during its turn, `wispd` stops the turn and shows the diff without reverting it. The check misses writes outside the repo and to ignored files.
- **Order:** Claude Code first, because one adapter covers a Max login and an Anthropic key, which is enough for M2. Codex second. Cursor ships only after Cursor confirms in writing (#35); if Cursor declines, its adapter is dropped. `CURSOR_API_KEY` is no fallback, because the AUP applies however the Service is accessed [34].

## Provider integration

Versions read: Claude Code 2.1.281 [19], Codex CLI 0.156.1 [27], Cursor CLI 2026.09.23 [42]. "Undocumented" means seen in real output but absent from the docs.

| | Claude Max | ChatGPT Plus | Cursor Pro+ |
| --- | --- | --- | --- |
| Binary | `claude` | `codex` | `agent`, with legacy name `cursor-agent` [42] |
| Headless run | `claude -p --output-format stream-json --verbose` [10] | `codex exec --json`, with stdin closed or holding the prompt [23][27] | `agent -p --output-format stream-json --trust` [38][41] |
| Events | `system` (`init`), `assistant`, `user`, `rate_limit_event`, `result` [14] | `thread.started`, `turn.started`, `item.started`/`updated`/`completed`, `turn.completed`, `turn.failed`, `error` [27] | `system` (`init`), `user`, `assistant`, `tool_call`, `result`; a failed run can end with no terminal event [39] |
| Resume | `--resume <session_id>` [10] | `codex exec resume <thread_id>` [23] | `--resume <chatId>` [38] |
| cwd and worktrees | Process cwd plus `--add-dir`; `-p` never shows the trust dialog [10][13] | `-C <dir>`; the dir must be in a git repo, and linked worktrees count [23][27] | `--workspace <dir>`; an untrusted folder fails without `--trust` [38][41] |
| Model | `--model` [10] | `-m` [27] | `--model`; list with `agent models` [38] |
| No-write mode | `--tools Read,Glob,Grep --setting-sources user --settings '{"disableAllHooks":true}' --strict-mcp-config --permission-mode dontAsk`. Write tools are removed, the project's settings, `env` block, and `.mcp.json` are skipped, and hooks are off, except managed-policy hooks [10][11][13]. `wispd`'s tools also need `--mcp-config` and `--allowedTools "mcp__wispd__*"`, or `dontAsk` denies them [10][11] | `-s read-only`, which Seatbelt enforces for commands [25]. `wispd`'s MCP tools need `mcp_servers.wispd.default_tools_approval_mode = "approve"` and no destructive hint [26], since exec denies approval requests [27] | `--mode ask --sandbox enabled`. Ask mode disables MCP execution, so Cursor cannot coordinate, and staff say modes "were never meant to be isolation" [44] |
| Tool needs approval | Denied, with a `permission_denied` event [11] | exec runs with approval policy `never`. An approval request is denied, so that call fails and the turn continues [27] | Held back unless `--force` or an allow rule is set; the docs disagree on whether edits are blocked or only proposed [36][38] |
| Cancel | SIGINT ends the turn; SIGTERM exits 143 and leaves the turn unfinished [11] | SIGINT interrupts the turn; there is no SIGTERM handler [27] | Undocumented; kill the process group |
| Signed in | `claude auth status`: JSON, exit 0 or 1 [10] | `codex login status`: text on stderr, exit 0 or 1 [27] | `agent status --format json` [38][40] |
| Plan | `subscriptionType` in `auth status`, undocumented and sometimes stale [18] | `planType` from `account/read` in `codex app-server` [22] | "Subscription Tier" in `agent about`, undocumented [47] |
| Login command | `claude auth login` [10] | `codex login`; on a remote machine, `--device-auth`, which is beta and must first be enabled in ChatGPT security settings [24] | `agent login`, with `NO_OPEN_BROWSER=1` on a remote machine [40] |
| Second account per machine | `CLAUDE_CONFIG_DIR` [12] | `CODEX_HOME` [24] | Unverified; the login lives in fixed Keychain items [46] |
| Tokens and cost | `result.modelUsage[model]`, which includes subagents and carries over into resumed sessions. `result.usage` covers the main loop only, and all costs are client-side estimates [14][15] | `turn.completed.usage`, cumulative for the thread [27] | `result.usage`: `inputTokens`, `outputTokens`, `cacheReadTokens`, `cacheWriteTokens`; no cost [41][43] |
| Limit windows | Only the last `rate_limit_event.rate_limit_info` seen: `status`, `rateLimitType` (`five_hour`, `seven_day`, ...), `utilization`, `resetsAt`. An experimental `get_usage` control request also returns the plan and windows [14] | `account/rateLimits/read` in `codex app-server`: `primary` and `secondary`, each with `usedPercent`, `windowDurationMins`, `resetsAt`; never in exec output [22] | None in headless output; `/usage` is interactive only; there is no public usage API for individual plans [41][45] |
| API key | `ANTHROPIC_API_KEY`, which wins over the login in `-p` [12]. Subscription runs strip `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, and `CLAUDE_CODE_OAUTH_TOKEN`, since all three outrank `/login` [12]. A project's `env` block can still set a key for worker runs [13], so `wispd` checks `apiKeySource` in `system/init` before charging a subscription account [14]. Direct calls: `POST https://api.anthropic.com/v1/messages` [17] | `CODEX_API_KEY` for exec [23]. Direct calls: `POST https://api.openai.com/v1/responses` [31] | `CURSOR_API_KEY` [40] runs the Cursor agent, not a model API [48], and falls under the same AUP question [34] |
| Remote Mac | A locked Keychain over SSH falls back to `~/.claude/.credentials.json` [12] | Stores `$CODEX_HOME/auth.json` by default [24][27] | Keychain writes fail over SSH; set `AGENT_CLI_CREDENTIAL_STORE=file` [46] |

## Terms assessment

| Approach | Claude Max | ChatGPT Plus | Cursor Pro+ |
| --- | --- | --- | --- |
| wisp signs in, or uses subscription tokens itself | Prohibited [1][5][6] | Unclear in the Terms [20]; endorsed by staff and the app-server docs [22][29][30]. wisp does not do it | Prohibited, with a ban risk [33][35] |
| wisp launches the user's own signed-in official CLI | Allowed if wisp meets the product conditions, including the Commercial Terms [1]; whether they apply to a local open-source launcher is unclear (#35) | Allowed [21][22][23] | Unclear. The AUP bans automated or scripted access with no CLI carve-out [34], while the CLI docs [36][37][40] and an Aug 10 staff post that predates the AUP [35] endorse it. Pending written confirmation (#35) |
| API key in the CLI or the vendor API | Allowed; the recommended path for third-party tools [1][3] | Allowed [23] | Unclear, for the same AUP reason [34]; agent-level only [48] |
| 2026 enforcement and changes | Jan: accounts whose third-party harnesses tripped abuse filters were banned, then anti-spoofing safeguards tightened [7]. By Feb 18: subscription tokens banned even in the Agent SDK, a ban removed by Apr 13 [2]. Apr 4: third-party harnesses moved to extra usage (secondary report [9]). Late Aug: the carve-out below [2] | None found | Aug 10: staff warn that token proxies risk a ban [35]. Aug 11: new AUP [34] |

- **Anthropic.** After barring third-party Claude.ai login and credential handling, the legal page adds: "Nor does it prevent an end user from signing in to the unmodified Claude Code binary with their own Claude subscription, including where a platform hosts Claude Code as described under *Can customers offer Claude Code in their products?* above" [1]. That section sets four conditions [1]:
  - accept the Commercial Terms;
  - leave the binary unmodified, with every sign-in method in place;
  - let each user sign in with their own credentials, without paying for or reselling their usage;
  - name Claude Code in plain text only.

  Other guidance cuts the other way. Product developers should use API keys [1]. Keys are "the preferred way" for third-party tools, "including open-source projects" [3]. Anthropic lets such tools use a plan only at its discretion, and only for subscribers who have enabled usage credits. It may then draw that use from usage credits instead of plan limits [3]. Third parties may not offer "claude.ai login or rate limits" [5].
- **OpenAI.** The Terms of Use ban programmatic extraction and credential sharing [20]. Even so, OpenAI documents `codex exec` and an app-server for "a deep integration inside your own product" [22][23], and its staff invite third-party ChatGPT sign-in [28][29][30].
- **Cursor.** Staff say that calling private endpoints with a user's token breaks ToS section 1.5 and "can trigger abuse enforcement, up to and including an account ban" [35]. The AUP bans "Accessing the Service through automated or non-human means, whether through a bot, script, or otherwise" [34], but the docs still point scripts at the CLI [36][40]. The staff post that endorses automation (Aug 10) predates the AUP; the Aug 16 post does not address automation [35].

## Backend interface

A sketch, with async omitted:

```rust
trait Backend {
    fn probe(&self, account: &Account) -> Probe;            // installed, version, signed in, auth kind, plan
    fn login_command(&self, account: &Account) -> Command;  // run in the editor terminal
    fn start(&self, account: &Account, task: Task) -> Run;  // cwd, prompt, model, tool policy, wispd MCP server, resume id
    fn limits(&self, account: &Account) -> Option<Limits>;  // latest windows and when they were read
}
trait Run {
    fn events(&mut self) -> Events; // Started, Text, ToolCall, FileChange, Usage, Limit, Finished, Failed
    fn cancel(&mut self);
}
```

What it smooths over, beyond the table's differences:

- **Approvals.** All three deny or fail in headless mode, so M2 fixes the policy up front.
- **Completion and counters.** A Cursor run can end with no terminal event, so completion comes from the exit code plus the last event. Cumulative totals become per-run deltas.
- **Commits.** Codex keeps `.git` read-only under `workspace-write`, including in worktrees [26], so `wispd` commits for Codex workers.

## Consequences

- Keys in the environment would reach the agent's shell, so `wispd` sets `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1` [16] and Codex's `shell_environment_policy.ignore_default_excludes=false` [26].
- On a remote Mac, SSH sessions may not reach the Keychain [12][46]. Running `wispd` as a LaunchAgent in the user's GUI session should avoid this, but that is unverified and requires the user to be logged in.

## Open risks

- **Claude billing.** Today `claude -p` and third-party app usage count against plan limits [4]. A plan announced in May [8] would have moved it to a monthly credit, then to usage credits if enabled, and otherwise stopped it. That plan is paused [4]. Anthropic may still require usage credits for third-party tools and bill their use there [3]. Limits assume "ordinary, individual usage" [1], which parallel subagents may exceed.
- **Commercial Terms.** A Claude Console account may satisfy this condition; unverified (#35).
- **Cursor.** Unsupported until #35 is answered. OpenAI plans to stop supplying models to Cursor on Nov 12, 2026 [32].
- **Schema churn.** Several fields are undocumented or experimental, including the Codex app-server [22]. Adapters pin tested CLI versions, replay recorded transcripts in CI, and ignore unknown fields.

## Sources

Read on 2026-09-23 unless dated.

Anthropic

1. Claude Code, Legal and compliance: https://code.claude.com/docs/en/legal-and-compliance
2. The same page on the Wayback Machine, 2026-02-01, 2026-02-18, 2026-04-13, 2026-08-16, and 2026-08-30: https://web.archive.org/web/20260201064220/https://code.claude.com/docs/en/legal-and-compliance, https://web.archive.org/web/20260218171531/https://code.claude.com/docs/en/legal-and-compliance, https://web.archive.org/web/20260413021834/https://code.claude.com/docs/en/legal-and-compliance, https://web.archive.org/web/20260816100739/https://code.claude.com/docs/en/legal-and-compliance, https://web.archive.org/web/20260830094710/https://code.claude.com/docs/en/legal-and-compliance
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
13. Claude Code permissions, "What runs before you trust a folder", and hooks, "Disable or remove hooks": https://code.claude.com/docs/en/permissions, https://code.claude.com/docs/en/hooks
14. Agent SDK types 0.3.281 (`SDKRateLimitInfo`, result message, `get_usage`): https://cdn.jsdelivr.net/npm/@anthropic-ai/claude-agent-sdk@0.3.281/sdk.d.ts
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
27. Codex CLI 0.156.1 release and source: https://github.com/openai/codex/releases/tag/rust-v0.156.1. Files under `codex-rs/` at that tag: `exec/src/exec_events.rs`, `exec/src/event_processor_with_jsonl_output.rs`, `exec/src/lib.rs`, `app-server/src/bespoke_event_handling.rs`, `cli/src/login.rs`, `config/src/types.rs`, `git-utils/src/info.rs`, `utils/cli/src/shared_options.rs`
28. Codex for Open Source: https://developers.openai.com/community/codex-for-oss
29. Sam Altman, 2026-05-01: https://x.com/sama/status/2050357911915028689
30. Tibo Sottiaux (OpenAI), 2026-05-23: https://x.com/thsottiaux/status/2058071172361998482
31. OpenAI Responses API: https://developers.openai.com/api/reference/resources/responses/methods/create
32. OpenAI, Our decision on Cursor following its acquisition by SpaceX, 2026-08-28 (the Nov 12 shutoff date is proposed): https://openai.com/index/our-decision-on-cursor-following-its-acquisition-by-spacex/

Cursor

33. Cursor Terms of Service, updated 2026-09-03: https://cursor.com/terms-of-service
34. Cursor Acceptable Use Policy, updated 2026-08-11: https://cursor.com/acceptable-use-policy
35. Cursor staff on official clients, 2026-08-10 (before the AUP) and 2026-08-16: https://forum.cursor.com/t/167778
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
