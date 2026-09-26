// A minimal JSON-RPC client for wispd (0007), for checks that need wispd to be in some state before
// they drive the app: it runs `wispd attach` and exchanges newline-delimited JSON over its stdio,
// the same transport the editor uses. It speaks only what a check needs; wisp's own client lives in
// the editor.
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';

interface Pending {
  resolve(result: unknown): void;
  reject(error: Error): void;
}

export class WispdError extends Error {
  readonly code: number;
  readonly kind: string | undefined;

  constructor(message: string, code: number, kind: string | undefined) {
    super(message);
    this.code = code;
    this.kind = kind;
  }
}

/** The `wispd` inside a packaged app, from the app's executable (`Wisp.app/Contents/MacOS/Wisp`). */
export function bundledWispd(executablePath: string): string {
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

  /** Runs `wispd attach` with `env`, which starts wispd there if it isn't running, and initializes. */
  static async attach(wispd: string, env: NodeJS.ProcessEnv): Promise<Wispd> {
    const client = new Wispd(spawn(wispd, ['attach'], { env, stdio: 'pipe' }));
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
    let message: { id?: unknown; result?: unknown; error?: { code: number; message: string; data?: { kind?: string } } };
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
      pending.reject(new WispdError(message.error.message, message.error.code, message.error.data?.kind));
    } else {
      pending.resolve(message.result);
    }
  }
}
