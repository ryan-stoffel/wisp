# wispd

wisp's host daemon. Each macOS user runs their own, and it keeps projects, and later agents, running while the editor is closed. The editor reaches it through `wispd attach`, either on the same Mac or on a host over SSH.

The `wisp` cask installs `wispd` at `Wisp.app/Contents/Resources/app/bin/wispd`, next to the `wisp` launcher, and links it into Homebrew's `bin` folder too (#62), so `wispd` on its own is a released build, and `wispd --version` prints the release's version. `scripts/editor/build-app` builds it with `cargo build --release -p wispd` for the app's architecture and copies it in before the bundle is signed; for a release, it also sets `WISP_VERSION`, which `daemon/src/main.rs` reads with `option_env!` at compile time (0006). A plain `cargo build -p wispd` outside that script has no `WISP_VERSION`, so `wispd --version` prints `Cargo.toml`'s own placeholder instead.

The decisions behind it:

- [0007](../docs/decisions/0007-editor-wispd-protocol.md): the protocol and transport.
- [0009](../docs/decisions/0009-wispd-data-folder-and-project-host.md): the data folder and `serve`.
- [0010](../docs/decisions/0010-wispd-attach.md): `attach`.

## Commands

| Command | What it does |
| --- | --- |
| `wispd serve` | Serves the protocol on this user's Unix socket until SIGTERM or SIGINT |
| `wispd attach` | Connects stdin and stdout to that socket, and starts wispd first if nothing is listening |
| `wispd service install`, `uninstall`, `status` | Manage the LaunchAgent that keeps `serve` running (#61) |

Both commands take `--data-dir` (or `WISPD_DATA_DIR`) to use a data folder other than `~/Library/Application Support/wisp`. `attach` also takes `--connect-timeout <seconds>`, which defaults to 10 and can be at most 86400, a day.

`attach` passes bytes through unchanged and prints nothing else on stdout. You can send a request by hand:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":{"min":1,"max":1},"client":{"name":"shell","version":"0"},"capabilities":{}}}' |
  wispd attach
```

When its input ends, `attach` keeps printing until wispd has answered everything it was sent, and then exits. The same command works through `ssh <host> wispd attach`.

If wispd isn't running, `attach` starts it through the LaunchAgent when one is installed and serves the same data folder. The LaunchAgent under the default label serves only the default data folder, so `wispd service install --data-dir <other>` needs `--label` as well. Otherwise it starts `wispd serve` in the background, in its own session. That `serve` keeps running after `attach` exits or the SSH connection drops, and `attach` never stops it.

### Exit codes

| Code | `serve` | `attach` |
| --- | --- | --- |
| 0 | Stopped cleanly | The connection ended |
| 1 | Couldn't start, or failed | Failed after connecting |
| 2 | Usage error | Usage error |
| 3 | Another `serve` already runs for this data folder | |
| 4 | | Never reached wispd |

`attach` writes the reason to stderr, along with the path of wispd's log, `logs/wispd.log` in the data folder.

## Using a Mac as a host over SSH

The editor on your laptop runs this command, where `<host>` is anything `ssh` accepts:

```sh
ssh -T -o BatchMode=yes -o ConnectTimeout=10 -o ControlPath=none -- <host> wispd attach
```

It uses your own ssh config, keys, and agent (0007). For that command to work, the host needs:

1. **wisp installed**, with `brew install --cask ryan-stoffel/taps/wisp`. The cask links `wispd` into Homebrew's `bin` folder (#62): `/opt/homebrew/bin` on Apple silicon, `/usr/local/bin` on Intel.
2. **`wispd` on the `PATH` of a non-interactive SSH command.** sshd runs `wispd attach` through your login shell as `zsh -c`, which reads `~/.zshenv` but not `~/.zprofile` or `~/.zshrc`. The `PATH` it starts with is `/usr/bin:/bin:/usr/sbin:/sbin`, so Homebrew's `bin` folder is missing. Fix it on the host in one of these ways. The examples use Apple silicon's `/opt/homebrew/bin`; on an Intel host, use `/usr/local/bin` instead.
   - Add Homebrew to `PATH` in `~/.zshenv`, the file zsh reads for every command:

     ```sh
     export PATH="/opt/homebrew/bin:$PATH"
     ```

   - Or run `wispd` by its full path, such as `/opt/homebrew/bin/wispd attach`. The editor retries with that path when the host's shell exits 127, meaning it didn't find `wispd` (#64).

   `SetEnv PATH=...` for the host in the laptop's `~/.ssh/config`, or `ssh -o SetEnv=...`, works only if the host's sshd lists `PATH` in `AcceptEnv`. macOS's sshd accepts only `LANG` and `LC_*`, so it drops `PATH`. Changing that means editing the host's sshd configuration, which affects every login, so prefer one of the fixes above.

   To check, run `ssh <host> 'command -v wispd'`.
3. **Quiet shell startup files.** Over SSH, stdout carries the protocol, so anything the host's shell prints while it starts a non-interactive command gets mixed into it. For zsh, that output comes from `~/.zshenv`.
   - Until `initialize` is answered, the editor skips lines that aren't JSON and logs them. After that, such a line is a protocol error (#64).
   - Keep output behind a check for an interactive shell, such as `[[ -o interactive ]]` in zsh.
4. **A key that logs in without prompts.** `BatchMode=yes` turns every prompt into an error. Run `ssh <host>` once in a terminal to accept the host key and unlock your key. Hosts that require interactive two-factor login aren't supported.
5. **The LaunchAgent, which is recommended** (#61). Without it, `attach` starts `wispd serve` itself.
   - A `serve` started that way inherits the SSH session's environment (#96).
   - A process started from SSH may not reach the Keychain, which the subscription CLIs use (0004).
   - The LaunchAgent runs wispd in your GUI session instead, and `attach` starts it with `launchctl kickstart` whenever it's installed.

wispd listens only on its Unix socket, never on a network port. SSH, with your own keys and config, is the only way in from another machine.

## Testing `attach` over ssh on your Mac

`cargo test -p wispd` tests `attach` through pipes, which are what ssh hands it. #95 adds a CI test through a real `ssh localhost`. To run one locally, you need these first:

- Remote Login turned on in System Settings > General > Sharing.
- A key authorized for your own account.
- localhost's host key accepted once.

Then run this from the repo root:

```sh
cargo build -p wispd
dir=$(mktemp -d /tmp/wispd-ssh.XXXXXX)
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":{"min":1,"max":1},"client":{"name":"ssh-test","version":"0"},"capabilities":{}}}' |
  ssh -T -o BatchMode=yes -o ControlPath=none -- localhost "WISPD_DATA_DIR=$dir $PWD/target/debug/wispd attach"
echo "exit: $?"
```

A successful answer with `"protocol":1` means the handshake went through ssh. The command exits 0 once wispd has answered, and the `serve` it started keeps running. Stop it with `kill "$(cat "$dir/wispd.lock")"`.
