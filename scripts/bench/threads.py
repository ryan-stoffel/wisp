#!/usr/bin/env python3
"""Starts N threads at once on a fresh plxd and times each one.

    scripts/bench/threads.py <plxd binary> <repo path> [threads] [model]

Runs `plxd serve` in a temporary data folder, adds a fresh clone of the repo, sends N `thread/start` requests at
the same moment, and reports per-thread start latency (request to response), time to first text,
and time to the first turn's end, plus plxd's CPU time. Needs a signed-in `claude` CLI.
"""

import asyncio
import json
import os
import secrets
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import uuid

PROMPT = "Reply with the numbers 1 to 40, one per line, and nothing else. Do not use any tools."


def uuid7() -> str:
    ms = time.time_ns() // 1_000_000
    raw = bytearray(ms.to_bytes(6, "big") + secrets.token_bytes(10))
    raw[6] = (raw[6] & 0x0F) | 0x70
    raw[8] = (raw[8] & 0x3F) | 0x80
    return str(uuid.UUID(bytes=bytes(raw)))


class Client:
    def __init__(self, reader, writer):
        self.reader, self.writer = reader, writer
        self.next_id = 0
        self.pending: dict[int, asyncio.Future] = {}
        self.events: asyncio.Queue = asyncio.Queue()
        asyncio.create_task(self.read())

    async def read(self):
        while line := await self.reader.readline():
            message = json.loads(line)
            if "id" in message and message["id"] in self.pending:
                self.pending.pop(message["id"]).set_result(message)
            elif message.get("method") == "events/event":
                await self.events.put((time.monotonic(), message["params"]))

    async def call(self, method, params):
        self.next_id += 1
        future = asyncio.get_running_loop().create_future()
        self.pending[self.next_id] = future
        frame = {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}
        self.writer.write(json.dumps(frame).encode() + b"\n")
        await self.writer.drain()
        message = await future
        if "error" in message:
            raise RuntimeError(f"{method}: {message['error']}")
        return message["result"]


async def connect(socket):
    for _ in range(200):
        try:
            client = Client(*await asyncio.open_unix_connection(socket, limit=64 << 20))
            await client.call(
                "initialize",
                {
                    "protocol": {"min": 1, "max": 1},
                    "client": {"name": "bench", "version": "0"},
                    "capabilities": {},
                },
            )
            return client
        except (FileNotFoundError, ConnectionRefusedError):
            await asyncio.sleep(0.05)
    raise RuntimeError("plxd never answered")


def pct(values, p):
    values = sorted(values)
    return values[min(len(values) - 1, round(p / 100 * (len(values) - 1)))]


async def main():
    plxd, source = sys.argv[1], os.path.abspath(sys.argv[2])
    count = int(sys.argv[3]) if len(sys.argv) > 3 else 20
    model = sys.argv[4] if len(sys.argv) > 4 else "claude-haiku-4-5"
    data = tempfile.mkdtemp(prefix="plxb-", dir="/tmp")
    env = {**os.environ, "PLXD_DATA_DIR": data}
    # A fresh clone each run: git's worktree commands slow down as a repo collects worktrees.
    repo = data + "-repo"
    subprocess.run(["git", "clone", "-q", "--no-hardlinks", source, repo], check=True)
    daemon = subprocess.Popen([plxd, "serve"], env=env, stderr=open(f"{data}.log", "w"))
    try:
        socket = os.path.join(data, "plxd.sock")
        client = await connect(socket)
        added = await client.call("repo/add", {"id": uuid7(), "path": repo})
        scope = added["repo"]["id"]
        await client.call("events/subscribe", {"after": 0, "project": scope})

        runs = {uuid7(): {} for _ in range(count)}
        t0 = time.monotonic()

        async def start(run_id):
            await client.call(
                "thread/start", {"runId": run_id, "repo": scope, "prompt": PROMPT, "model": model,
                 "account": {"kind": "subscription", "backend": "claude"}}
            )
            runs[run_id]["start"] = time.monotonic() - t0

        starts = asyncio.gather(*(start(run_id) for run_id in runs))
        starts.add_done_callback(lambda f: f.exception() and os._exit(print(f.exception()) or 1))
        done = 0
        output_events = 0
        while done < count:
            at, params = await asyncio.wait_for(client.events.get(), timeout=300)
            event = params["event"]
            run = runs.get(event.get("runId"))
            if run is None:
                continue
            if event["kind"] == "agent.finished" and "done" not in run:
                run["done"] = at - t0
                run["failed"] = event.get("outcome", {})
                done += 1
            if event["kind"] != "agent.output":
                continue
            output_events += 1
            for item in event["items"]:
                if item["kind"] in ("text", "textDelta"):
                    run.setdefault("first", at - t0)
                if item["kind"] == "turnFinished" and "done" not in run:
                    run["done"] = at - t0
                    done += 1
        await starts
        wall = time.monotonic() - t0
        cpu = subprocess.run(
            ["ps", "-o", "cputime=", "-p", str(daemon.pid)], capture_output=True, text=True
        ).stdout.strip()

        failed = [r["failed"] for r in runs.values() if r.get("failed")]
        print(f"threads={count} model={model} wall={wall:.2f}s plxd_cpu={cpu} output_events={output_events}")
        if failed:
            print(f"finished early: {failed[:3]}")
        for key, label in (("start", "thread/start"), ("first", "first text"), ("done", "turn done")):
            values = [r[key] for r in runs.values() if key in r]
            if values:
                print(
                    f"{label:>13}: p50={statistics.median(values):6.2f}s "
                    f"p90={pct(values, 90):6.2f}s max={max(values):6.2f}s n={len(values)}"
                )
    finally:
        daemon.terminate()
        daemon.wait()
        shutil.rmtree(data, ignore_errors=True)
        shutil.rmtree(repo, ignore_errors=True)


asyncio.run(main())
