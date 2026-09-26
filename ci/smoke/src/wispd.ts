// A minimal JSON-RPC client for wispd (0007) over `wispd attach`'s stdio, for checks that need
// wispd in some state before they drive the app, or that read wispd's own state directly.
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';

interface Pending {
  resolve(result: unknown): void;
  reject(error: Error): void;
}

/** The `wispd` inside a packaged app, from scripts/ci/app-launch's `executablePath` (`Wisp.app/Contents/MacOS/Wisp`). */
export function bundledWispd({ executablePath }: { executablePath?: string }): string {
  if (!executablePath) {
    throw new Error('app-launch gave no executablePath');
  }
  return join(dirname(executablePath), '..', 'Resources', 'app', 'bin', 'wispd');
}

/** A version 7 UUID, as the protocol's client-generated ids are. */
export function uuidV7(): string {
  const bytes = randomBytes(16);
  bytes.writeUIntBE(Date.now(), 0, 6);
  bytes[6] = 0x70 | ((bytes[6] ?? 0) & 0x0f);
  bytes[8] = 0x80 | ((bytes[8] ?? 0) & 0x3f);
  const hex = bytes.toString('hex');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

export class Wispd {
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private stderr = '';
  private readonly child: ChildProcessWithoutNullStreams;

  private constructor(child: ChildProcessWithoutNullStreams) {
    this.child = child;
    createInterface({ input: child.stdout }).on('line', (line) => {
      this.receive(line);
    });
    child.stderr.on('data', (chunk: Buffer) => {
      this.stderr = (this.stderr + chunk.toString('utf8')).slice(-4096);
    });
    child.on('exit', () => {
      for (const pending of this.pending.values()) {
        pending.reject(new Error(`wispd attach exited: ${this.stderr}`));
      }
      this.pending.clear();
    });
  }

  /**
   * Runs `<command> attach` (or `command` with `args`, such as an ssh session running one) with
   * `env`, which starts wispd there if it isn't running, and initializes.
   */
  static async attach(command: string, env: NodeJS.ProcessEnv, args: readonly string[] = ['attach']): Promise<Wispd> {
    const client = new Wispd(spawn(command, args, { env, stdio: 'pipe' }));
    await client.request('initialize', {
      protocol: { min: 1, max: 1 },
      client: { name: 'wisp-smoke', version: '0.0.0' },
      capabilities: {},
    });
    return client;
  }

  request<T = unknown>(method: string, params: unknown): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: (result) => { resolve(result as T); }, reject });
      this.child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
    });
  }

  close(): void {
    this.child.stdin.end();
    this.child.kill();
  }

  private receive(line: string): void {
    let message: { id?: unknown; result?: unknown; error?: { message: string } };
    try {
      message = JSON.parse(line) as typeof message;
    } catch {
      return;
    }
    if (typeof message.id !== 'number') {
      return;
    }
    const pending = this.pending.get(message.id);
    if (!pending) {
      return;
    }
    this.pending.delete(message.id);
    if (message.error) {
      pending.reject(new Error(message.error.message));
    } else {
      pending.resolve(message.result);
    }
  }
}
