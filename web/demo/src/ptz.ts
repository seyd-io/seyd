// PTZ operator controls, ported from the prototype pilot: canvas drag is a
// virtual joystick (12% deadzone, speed ramped from the deadzone edge), arrow
// keys pan/tilt, Shift is fast, wheel and +/- zoom, H goes home. Velocity, not
// position; repeated every 200 ms while held so a lost stop costs one expiry
// window on the robot. Stop on blur and tab-hide.
import type { SeydSession } from '@seyd/core';

const REPEAT_MS = 200;
const DEADZONE = 0.12;
const SPEED = 55;
const SPEED_FAST = 100;
const KEYS = new Set(['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Equal', 'Minus']);

interface Vec { pan: number; tilt: number; zoom: number }

export class PtzController {
  private enabled = false;
  private timer: ReturnType<typeof setInterval> | null = null;
  private sent: Vec = { pan: 0, tilt: 0, zoom: 0 };
  private pointerVec: { pan: number; tilt: number } | null = null;
  private wheelZoom = 0;
  private wheelTimer: ReturnType<typeof setTimeout> | null = null;
  private held = new Set<string>();
  private cleanups: (() => void)[] = [];

  constructor(private session: SeydSession, private canvas: HTMLCanvasElement, private surface: HTMLElement) {
    const on = <K extends keyof HTMLElementEventMap>(el: HTMLElement | Document | Window, t: string, h: (e: never) => void, o?: AddEventListenerOptions) => {
      el.addEventListener(t, h as EventListener, o); this.cleanups.push(() => el.removeEventListener(t, h as EventListener, o));
      void (0 as unknown as K);
    };
    on(surface, 'pointerdown', (e: PointerEvent) => {
      if (!this.enabled) return;
      e.preventDefault();
      try { surface.setPointerCapture(e.pointerId); } catch { /* ignore */ }
      this.pointerVec = this.pointerVector(e); this.refresh();
    });
    on(surface, 'pointermove', (e: PointerEvent) => { if (!this.pointerVec) return; this.pointerVec = this.pointerVector(e); this.refresh(); });
    for (const t of ['pointerup', 'pointercancel', 'lostpointercapture']) on(surface, t, () => { if (!this.pointerVec) return; this.pointerVec = null; this.refresh(); });
    on(surface, 'wheel', (e: WheelEvent) => {
      if (!this.enabled) return;
      e.preventDefault();
      this.wheelZoom = e.deltaY < 0 ? SPEED : -SPEED; this.refresh();
      if (this.wheelTimer) clearTimeout(this.wheelTimer);
      this.wheelTimer = setTimeout(() => { this.wheelZoom = 0; this.refresh(); }, 220);
    }, { passive: false });
    on(document, 'keydown', (e: KeyboardEvent) => {
      if (e.repeat) { if (KEYS.has(e.code)) e.preventDefault(); return; }
      if (e.key === 'Shift') { this.held.add('Shift'); this.refresh(); return; }
      if (KEYS.has(e.code)) { e.preventDefault(); if (!this.enabled) return; this.held.add(e.code); if (e.shiftKey) this.held.add('Shift'); this.refresh(); return; }
      if (e.code === 'KeyH' && this.enabled) { e.preventDefault(); this.session.send('ptz', { home: true, ts: Date.now() }); }
    });
    on(document, 'keyup', (e: KeyboardEvent) => {
      if (e.key === 'Shift') { this.held.delete('Shift'); this.refresh(); return; }
      if (KEYS.has(e.code)) { this.held.delete(e.code); this.refresh(); }
    });
    on(window, 'blur', () => this.stop());
    on(document, 'visibilitychange', () => { if (document.hidden) this.stop(); });
  }

  setEnabled(v: boolean): void { this.enabled = v; this.surface.style.cursor = v ? 'crosshair' : ''; if (!v) this.stop(); }

  dispose(): void { this.stop(); this.cleanups.forEach((c) => c()); this.cleanups = []; }

  private pointerVector(e: PointerEvent): { pan: number; tilt: number } {
    const r = this.canvas.getBoundingClientRect();
    if (!r.width || !r.height) return { pan: 0, tilt: 0 };
    const nx = ((e.clientX - r.left) / r.width) * 2 - 1;
    const ny = ((e.clientY - r.top) / r.height) * 2 - 1;
    const mag = Math.hypot(nx, ny);
    if (mag < DEADZONE) return { pan: 0, tilt: 0 };
    const ramp = Math.min(1, (mag - DEADZONE) / (1 - DEADZONE)) / mag;
    return { pan: nx * ramp * 100, tilt: -ny * ramp * 100 };
  }

  private keyboardVector(): Vec {
    const s = this.held.has('Shift') ? SPEED_FAST : SPEED;
    let pan = 0, tilt = 0, zoom = 0;
    if (this.held.has('ArrowLeft')) pan -= s; if (this.held.has('ArrowRight')) pan += s;
    if (this.held.has('ArrowUp')) tilt += s; if (this.held.has('ArrowDown')) tilt -= s;
    if (this.held.has('Equal')) zoom += s; if (this.held.has('Minus')) zoom -= s;
    return { pan, tilt, zoom };
  }

  private refresh(): void {
    const k = this.keyboardVector();
    const zoom = this.wheelZoom || k.zoom;
    if (this.pointerVec) this.set(this.pointerVec.pan, this.pointerVec.tilt, zoom); else this.set(k.pan, k.tilt, zoom);
  }

  private stop(): void { this.held.clear(); this.pointerVec = null; this.wheelZoom = 0; this.set(0, 0, 0); }

  private set(pan: number, tilt: number, zoom: number): void {
    const clamp = (v: number) => Math.max(-100, Math.min(100, Math.round(v || 0)));
    const next: Vec = { pan: clamp(pan), tilt: clamp(tilt), zoom: clamp(zoom) };
    const changed = next.pan !== this.sent.pan || next.tilt !== this.sent.tilt || next.zoom !== this.sent.zoom;
    const moving = next.pan !== 0 || next.tilt !== 0 || next.zoom !== 0;
    this.sent = next;
    if (!moving || !this.enabled) { if (this.timer) clearInterval(this.timer); this.timer = null; }
    if (!this.enabled) return;
    if (moving) {
      if (changed) this.send();
      if (!this.timer) this.timer = setInterval(() => this.send(), REPEAT_MS);
      return;
    }
    if (changed) this.send();   // one explicit stop
  }

  private send(): void { this.session.send('ptz', { ...this.sent, ts: Date.now() }); }
}
