// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// The control stream: NDJSON on one pilot-opened bidirectional stream.
// The pilot speaks first (docs/protocol/control-stream.md).
import { Transport } from './transport.js';

export type ControlMessage = { type: string; [k: string]: unknown };

export class ControlStream {
  private writer: WritableStreamDefaultWriter<Uint8Array> | null = null;
  private enc = new TextEncoder();
  private closed = false;

  constructor(private onMessage: (m: ControlMessage) => void, private onClosed: () => void) {}

  async open(t: Transport): Promise<void> {
    const stream = await t.openControl();
    this.writer = stream.writable.getWriter();
    void this.readLoop(stream.readable);
  }

  private async readLoop(readable: ReadableStream<Uint8Array>): Promise<void> {
    const reader = readable.getReader();
    const dec = new TextDecoder();
    let buf = '';
    try {
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        const lines = buf.split('\n');
        buf = lines.pop() ?? '';
        for (const line of lines) {
          if (!line.trim()) continue;
          try { this.onMessage(JSON.parse(line)); } catch { /* malformed line: ignore */ }
        }
      }
    } catch { /* stream reset */ }
    if (!this.closed) { this.closed = true; this.onClosed(); }
  }

  send(msg: ControlMessage): void {
    if (!this.writer || this.closed) return;
    this.writer.write(this.enc.encode(JSON.stringify(msg) + '\n')).catch(() => { /* closed */ });
  }

  async close(): Promise<void> {
    this.closed = true;
    try { await this.writer?.close(); } catch { /* ignore */ }
    this.writer = null;
  }
}
