import { ArrowUpRight, Check, Copy, LoaderCircle, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useState, type ReactNode } from "react";

import type { GithubStatus } from "../../protocol/generated/protocol";
import { useCopy } from "../AgentChat";
import { statusLabel, useConnection } from "../ConnectionStatus";
import { describeError } from "../errors";
import { localId, useHosts, type Host } from "../hosts";
import { GitHubLogo } from "../logos";
import { IconButton } from "../ui";
import {
  HostPicker,
  PageTitle,
  primaryButton,
  quietButton,
  Section,
  settingRow,
  StatusDot,
} from "./parts";

/** How often the status is read while plxd installs `gh` or waits on a sign-in. */
const POLL_MS = 2000;

/**
 * Settings > Source control: GitHub on a host, through the `gh` CLI plxd runs there for pull
 * requests: whether it's installed, its version, and the account it's signed in to. On a plxd with
 * `githubSetup` (PLX-423), the row installs `gh` and signs it in. `hostId` picks the host it opens
 * on, as Set up GitHub does; this computer otherwise.
 */
export function SourceControlSettings({ hostId: opened }: { hostId?: string }) {
  const hosts = useHosts();
  const [hostId, setHostId] = useState(opened ?? localId);
  const host = hosts.find((h) => h.id === hostId) ?? hosts[0]!;
  return (
    <>
      <PageTitle title="Source control">
        Parallax opens and follows pull requests with the GitHub CLI on each host.
      </PageTitle>
      <GitHub key={host.id} host={host} hosts={hosts} onHost={setHostId} />
    </>
  );
}

function GitHub({
  host,
  hosts,
  onHost,
}: {
  host: Host;
  hosts: Host[];
  onHost: (id: string) => void;
}) {
  const connection = useConnection(host.id);
  const connected = connection?.status === "connected";
  // A plxd from before `github/status` can't say, and one from before `githubSetup` can't set up.
  const supported = connected && "githubStatus" in connection.capabilities;
  const setup = connected && "githubSetup" in connection.capabilities;
  const [status, setStatus] = useState<GithubStatus>();
  const [error, setError] = useState<string>();
  const [checking, setChecking] = useState(false);
  // Why Install, Sign in, or Cancel was refused.
  const [failed, setFailed] = useState<string>();
  const [starting, setStarting] = useState(false);
  const [copied, copy] = useCopy();

  const load = useCallback(
    async (quiet = false) => {
      if (!quiet) setChecking(true);
      const answer = await window.parallax.request(host.id, "github/status", {});
      if (!quiet) setChecking(false);
      if ("error" in answer) setError(describeError(answer.error));
      else {
        setStatus(answer.result);
        setError(undefined);
      }
    },
    [host.id],
  );
  useEffect(() => {
    if (supported) void load();
  }, [supported, load]);
  // The row moves on by itself once the install ends or the browser approves the code.
  const waiting = !!status?.installing || !!status?.signingIn;
  useEffect(() => {
    if (!waiting) return;
    const timer = setInterval(() => void load(true), POLL_MS);
    return () => clearInterval(timer);
  }, [waiting, load]);

  const install = async () => {
    setFailed(undefined);
    const answer = await window.parallax.request(host.id, "github/install", {});
    if ("error" in answer) setFailed(describeError(answer.error));
    else setStatus(answer.result);
  };
  const signIn = async () => {
    setFailed(undefined);
    setStarting(true);
    const answer = await window.parallax.request(host.id, "github/signIn", {});
    setStarting(false);
    if ("error" in answer) return setFailed(describeError(answer.error));
    setStatus((s) => s && { ...s, signingIn: answer.result, setupNote: undefined });
    // The device page opens here even for an SSH host, whose browser is this computer's.
    window.open(answer.result.url, "_blank");
  };
  const cancel = async () => {
    const answer = await window.parallax.request(host.id, "github/signInCancel", {});
    if ("error" in answer) setFailed(describeError(answer.error));
    await load(true);
  };

  let detail: string;
  let tone: "on" | "off" | "warn" = "warn";
  if (!connected) detail = connection ? statusLabel(connection) : "Connecting…";
  else if (!supported) detail = "Update plxd on this host to see GitHub here.";
  else if (error) detail = error;
  else if (!status) detail = "Checking…";
  else if (status.installing) detail = "Installing the GitHub CLI…";
  else if (!status.installed) {
    detail = setup
      ? "Not installed. Parallax can install the GitHub CLI on this host."
      : "Not installed. Install the GitHub CLI on this host to open pull requests.";
    tone = "off";
  } else if (status.signingIn) detail = "Waiting for you to approve the code on GitHub…";
  else if (status.signedIn === false)
    detail = setup
      ? "Not signed in."
      : "Not signed in. Run `gh auth login` on this host, then refresh.";
  else if (status.signedIn) {
    detail = status.account ? `Signed in as @${status.account}` : "Signed in";
    tone = "on";
  } else detail = status.note ?? "Couldn't tell whether gh is signed in.";

  let action: ReactNode = null;
  if (status && !error && setup) {
    if (status.installing)
      action = (
        <span className={`${quietButton} flex items-center gap-1.5 [&_svg]:size-3.5`}>
          <LoaderCircle aria-hidden className="animate-spin" />
          Installing
        </span>
      );
    else if (!status.installed)
      action = (
        <button type="button" className={primaryButton} onClick={() => void install()}>
          Install
        </button>
      );
    else if (status.signingIn)
      action = (
        <button type="button" className={quietButton} onClick={() => void cancel()}>
          Cancel
        </button>
      );
    else if (status.signedIn === false)
      action = (
        <button
          type="button"
          className={primaryButton}
          disabled={starting}
          onClick={() => void signIn()}
        >
          {starting ? "Starting…" : "Sign in"}
        </button>
      );
  } else if (status && !status.installed)
    action = (
      <a
        href="https://cli.github.com"
        target="_blank"
        rel="noreferrer"
        className={`${quietButton} flex shrink-0 items-center gap-1 [&_svg]:size-3.5`}
      >
        Install
        <ArrowUpRight aria-hidden />
      </a>
    );

  const pending = setup ? status?.signingIn : undefined;
  const note = failed ?? (setup ? status?.setupNote : undefined);
  return (
    <Section
      title="Hosting"
      action={
        <div className="flex items-center gap-2">
          <HostPicker hosts={hosts} value={host.id} onChange={onHost} />
          <IconButton label="Refresh" disabled={!supported || checking} onClick={() => void load()}>
            <RefreshCw aria-hidden className={checking ? "animate-spin" : undefined} />
          </IconButton>
        </div>
      }
    >
      <div className={settingRow}>
        <div className="flex min-w-0 items-start gap-3">
          <GitHubLogo className="mt-0.5 size-5 shrink-0" />
          <div className="min-w-0">
            <span className="flex items-baseline gap-2">
              <span className="text-[13px] font-medium">GitHub</span>
              {status?.version && (
                <span className="truncate font-mono text-[11.5px] text-faint-foreground">
                  gh {status.version}
                  {status.managed && " · installed by Parallax"}
                </span>
              )}
            </span>
            <span
              role={error ? "alert" : undefined}
              className="flex items-center gap-1.5 text-[12.5px] text-muted-foreground"
            >
              <StatusDot tone={tone} />
              {detail}
            </span>
            {note && (
              <span role="alert" className="mt-1 block text-[12.5px] text-warning">
                {note}
              </span>
            )}
          </div>
        </div>
        {action && <div className="flex shrink-0 items-center gap-2">{action}</div>}
      </div>
      {pending && (
        <div className={`${settingRow} flex-wrap`}>
          <div className="min-w-0">
            <span className="block text-[12.5px] text-muted-foreground">
              Enter this code at {pending.url.replace(/^https:\/\//, "")}. It expires at{" "}
              {new Date(pending.expiresAt).toLocaleTimeString([], {
                hour: "numeric",
                minute: "2-digit",
              })}
              .
            </span>
            <span
              aria-label="One-time code"
              className="mt-1 block font-mono text-xl font-semibold tracking-widest select-all"
            >
              {pending.code}
            </span>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            <button
              type="button"
              className={`${quietButton} flex items-center gap-1 [&_svg]:size-3.5`}
              onClick={() => copy(pending.code)}
            >
              {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
              {copied ? "Copied" : "Copy"}
            </button>
            <button
              type="button"
              className={`${quietButton} flex items-center gap-1 [&_svg]:size-3.5`}
              onClick={() => window.open(pending.url, "_blank")}
            >
              Open GitHub
              <ArrowUpRight aria-hidden />
            </button>
          </div>
        </div>
      )}
    </Section>
  );
}
