// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// Flight controls for a robot that declares a `flight` command channel (the
// Tello demo, DEMO-TELLO.md). Four stick axes as velocities in -100..100:
// arrows move (pitch/roll), R/F climb and descend, Q/E turn, Shift is fast;
// T takes off, L lands. A drag on the picture is a virtual joystick for
// pitch/roll (12% deadzone), for touch. W/A/S/D are deliberately unused: S
// toggles the HUD and Space takes a snapshot everywhere on the page.
//
// Velocity, not position, repeated every 100 ms while anything is held, so
// the robot's stick hold (400 ms on the Tello bridge) is renewed several times
// over and a lost stop costs at most one hold window: the drone centres its
// sticks and hovers. Stop on blur and tab-hide, and one explicit zero on
// release.
import type { SeydSession } from '@seyd/core';

const REPEAT_MS = 100;
// While the controls are live and the tab is visible, the current sticks are
// sent at least this often even when centred. It is the pilot's presence: the
// bridge lands a drone whose driver has been silent for five seconds, and a
// pilot hovering with hands off the keys is not silent, only still. A hidden
// tab stops it on purpose — a pilot who cannot see the picture is not flying.
const PRESENCE_MS = 1000;
const DEADZONE = 0.12;
const SPEED = 35;
const SPEED_FAST = 70;
const AXIS_KEYS = new Set(['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'KeyR', 'KeyF', 'KeyQ', 'KeyE']);

export interface Sticks { roll: number; pitch: number; throttle: number; yaw: number }

export class FlightController {
  private enabled = false;
  private timer: ReturnType<typeof setInterval> | null = null;
  private presence: ReturnType<typeof setInterval> | null = null;
  private sent: Sticks = { roll: 0, pitch: 0, throttle: 0, yaw: 0 };
  private pointerVec: { roll: number; pitch: number } | null = null;
  private padVec: Sticks | null = null;
  private held = new Set<string>();
  private cleanups: (() => void)[] = [];

  constructor(private session: SeydSession, private canvas: HTMLCanvasElement, private surface: HTMLElement) {
    const on = (el: HTMLElement | Document | Window, t: string, h: (e: never) => void, o?: AddEventListenerOptions) => {
      el.addEventListener(t, h as EventListener, o); this.cleanups.push(() => el.removeEventListener(t, h as EventListener, o));
    };
    on(surface, 'pointerdown', (e: PointerEvent) => {
      if (!this.enabled) return;
      e.preventDefault();
      try { surface.setPointerCapture(e.pointerId); } catch { /* ignore */ }
      this.pointerVec = this.pointerVector(e); this.refresh();
    });
    on(surface, 'pointermove', (e: PointerEvent) => { if (!this.pointerVec) return; this.pointerVec = this.pointerVector(e); this.refresh(); });
    for (const t of ['pointerup', 'pointercancel', 'lostpointercapture']) on(surface, t, () => { if (!this.pointerVec) return; this.pointerVec = null; this.refresh(); });
    on(document, 'keydown', (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.repeat) { if (AXIS_KEYS.has(e.code) && this.enabled) e.preventDefault(); return; }
      if (e.key === 'Shift') { this.held.add('Shift'); this.refresh(); return; }
      if (AXIS_KEYS.has(e.code)) { if (!this.enabled) return; e.preventDefault(); this.held.add(e.code); if (e.shiftKey) this.held.add('Shift'); this.refresh(); return; }
      if (e.code === 'KeyT' && this.enabled) { e.preventDefault(); this.takeoff(); }
      if (e.code === 'KeyL' && this.enabled) { e.preventDefault(); this.land(); }
    });
    on(document, 'keyup', (e: KeyboardEvent) => {
      if (e.key === 'Shift') { this.held.delete('Shift'); this.refresh(); return; }
      if (AXIS_KEYS.has(e.code)) { this.held.delete(e.code); this.refresh(); }
    });
    on(window, 'blur', () => this.stop());
    on(document, 'visibilitychange', () => { if (document.hidden) this.stop(); });
  }

  setEnabled(v: boolean): void {
    this.enabled = v; this.surface.style.cursor = v ? 'crosshair' : ''; if (!v) this.stop();
    if (this.presence) { clearInterval(this.presence); this.presence = null; }
    if (v) this.presence = setInterval(() => { if (!document.hidden && !this.timer) this.send(); }, PRESENCE_MS);
  }

  dispose(): void { this.setEnabled(false); this.cleanups.forEach((c) => c()); this.cleanups = []; }

  takeoff(): void { if (this.enabled) this.session.send('flight', { takeoff: true, ts: Date.now() }); }

  /** Land is the stop button: sticks centred first, then the command, and it works even while a key is held. */
  land(): void { if (!this.enabled) return; this.stop(); this.session.send('flight', { land: true, ts: Date.now() }); }

  private pointerVector(e: PointerEvent): { roll: number; pitch: number } {
    const r = this.canvas.getBoundingClientRect();
    if (!r.width || !r.height) return { roll: 0, pitch: 0 };
    const nx = ((e.clientX - r.left) / r.width) * 2 - 1;
    const ny = ((e.clientY - r.top) / r.height) * 2 - 1;
    const mag = Math.hypot(nx, ny);
    if (mag < DEADZONE) return { roll: 0, pitch: 0 };
    const ramp = Math.min(1, (mag - DEADZONE) / (1 - DEADZONE)) / mag;
    const s = this.held.has('Shift') ? SPEED_FAST : SPEED;
    return { roll: nx * ramp * s, pitch: -ny * ramp * s };
  }

  private keyboardVector(): Sticks {
    const s = this.held.has('Shift') ? SPEED_FAST : SPEED;
    let roll = 0, pitch = 0, throttle = 0, yaw = 0;
    if (this.held.has('ArrowLeft')) roll -= s; if (this.held.has('ArrowRight')) roll += s;
    if (this.held.has('ArrowUp')) pitch += s; if (this.held.has('ArrowDown')) pitch -= s;
    if (this.held.has('KeyR')) throttle += s; if (this.held.has('KeyF')) throttle -= s;
    if (this.held.has('KeyQ')) yaw -= s; if (this.held.has('KeyE')) yaw += s;
    return { roll, pitch, throttle, yaw };
  }

  /** Sticks from a gamepad (main.ts maps the pad); a non-zero pad axis overrides keyboard and drag on that axis. */
  setPad(v: Sticks | null): void {
    const was = this.padVec;
    this.padVec = v && (v.roll || v.pitch || v.throttle || v.yaw) ? v : null;
    if (this.padVec || was) this.refresh();
  }

  private refresh(): void {
    const k = this.keyboardVector();
    const base: Sticks = this.pointerVec ? { ...k, roll: this.pointerVec.roll, pitch: this.pointerVec.pitch } : k;
    const p = this.padVec;
    this.set(p ? { roll: p.roll || base.roll, pitch: p.pitch || base.pitch, throttle: p.throttle || base.throttle, yaw: p.yaw || base.yaw } : base);
  }

  private stop(): void { this.held.clear(); this.pointerVec = null; this.padVec = null; this.set({ roll: 0, pitch: 0, throttle: 0, yaw: 0 }); }

  private set(v: Sticks): void {
    const clamp = (x: number) => Math.max(-100, Math.min(100, Math.round(x || 0)));
    const next: Sticks = { roll: clamp(v.roll), pitch: clamp(v.pitch), throttle: clamp(v.throttle), yaw: clamp(v.yaw) };
    const changed = (Object.keys(next) as (keyof Sticks)[]).some((k) => next[k] !== this.sent[k]);
    const moving = next.roll !== 0 || next.pitch !== 0 || next.throttle !== 0 || next.yaw !== 0;
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

  private send(): void { this.session.send('flight', { ...this.sent, ts: Date.now() }); }
}

interface Telemetry {
  drone?: string; flying?: boolean; battery?: number; battery_low?: boolean; height_m?: number; speed_mps?: number;
  yaw_deg?: number; fly_time_s?: number; wifi?: number; wind?: boolean; hot?: boolean; notice?: string;
  video?: { fps?: number; kbps?: number; lost_frames?: number };
}

/** The Tello bridge's telemetry as one footer line, or null if the message is not that shape. */
export function formatTelemetry(data: unknown): string | null {
  if (!data || typeof data !== 'object' || !('battery' in data) || !('flying' in data)) return null;
  const t = data as Telemetry;
  if (t.drone && t.drone !== 'connected') return 'drone: not connected';
  const mmss = (s: number) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`;
  const parts = [
    t.notice ? `⚠ ${t.notice}` : '',
    `BAT ${t.battery ?? '?'}%${t.battery_low ? ' LOW' : ''}`,
    t.flying ? `ALT ${(t.height_m ?? 0).toFixed(1)} m` : 'on the ground',
    t.flying ? `SPD ${(t.speed_mps ?? 0).toFixed(1)} m/s` : '',
    t.flying ? `HDG ${t.yaw_deg ?? 0}°` : '',
    t.flying ? `T+${mmss(t.fly_time_s ?? 0)}` : '',
    `WIFI ${t.wifi ?? '?'}`,
    t.wind ? 'WIND' : '',
    t.hot ? 'HOT' : '',
    t.video ? `${t.video.fps ?? 0} fps ${t.video.kbps ?? 0} kbps${t.video.lost_frames ? ` lost ${t.video.lost_frames}` : ''}` : '',
  ];
  return parts.filter(Boolean).join(' · ');
}
