// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
// A USB gamepad for the pilot page, through the browser's Gamepad API. The
// reader polls whatever pad the browser exposes (Chrome shows one only after
// a button has been pressed on it) and turns it into one scheme-neutral
// `PadInput`: a move vector, a look vector, turn/up/down/fast holds and
// start/select presses. main.ts maps that onto whichever controller is live —
// PTZ or flight — so the pad knows nothing about drones or cameras.
//
// Two layouts are understood. A "standard"-mapped pad (the W3C layout: face
// buttons 0 bottom, 1 right, 2 left, 3 top; 4/5 shoulders, 6/7 triggers, 8
// select, 9 start, d-pad on 12–15 or on axes 0/1) — which is what Chrome
// assigns the iBuffalo Classic USB too (verified on the device 2026-10-04:
// B=0 A=1 Y=2 X=3, L=4+6, R=5+7, Select=8, Start=9) — and, as a fallback,
// an unmapped 2-axis 8-button pad read in HID order A, B, X, Y, L, R,
// Select, Start. `?pad=debug` shows the raw axes and buttons on the footer,
// which is how a third layout gets its table.

export interface PadInput {
  id: string;
  /** Primary direction, -1..1 each; +x right, +y forward. D-pad or left stick. */
  move: { x: number; y: number };
  /** Secondary direction, -1..1; right stick only (0 on a d-pad-only pad). */
  look: { x: number; y: number };
  turnLeft: boolean; turnRight: boolean;   // shoulders
  up: boolean; down: boolean;              // X / B on SNES; triggers on standard
  fast: boolean;                           // A on SNES; A/cross on standard
  start: boolean; select: boolean;         // edge-triggered: true for the frame they were pressed
}

const DEADZONE = 0.15;

function axis(v: number | undefined): number {
  const x = v ?? 0;
  if (Math.abs(x) < DEADZONE) return 0;
  const sign = x < 0 ? -1 : 1;
  return sign * Math.min(1, (Math.abs(x) - DEADZONE) / (1 - DEADZONE));
}

function pressed(gp: Gamepad, i: number): boolean {
  const b = gp.buttons[i];
  return !!b && (b.pressed || b.value > 0.5);
}

export function describe(gp: Gamepad): string {
  return `${gp.id.replace(/\s*\(.*$/, '')} · axes ${gp.axes.map((a) => a.toFixed(1)).join(' ')} · buttons ${gp.buttons.map((b, i) => (b.pressed ? i : '·')).join(' ')}`;
}

type Held = { start: boolean; select: boolean };

function isStandard(gp: Gamepad): boolean { return gp.mapping === 'standard' || gp.axes.length >= 4; }

/** The start/select buttons as currently held, for edge detection across frames. */
function held(gp: Gamepad): Held {
  return isStandard(gp) ? { start: pressed(gp, 9), select: pressed(gp, 8) } : { start: pressed(gp, 7), select: pressed(gp, 6) };
}

function read(gp: Gamepad, prev: Held): PadInput {
  const { start, select } = held(gp);
  if (isStandard(gp)) {
    const dpadX = (pressed(gp, 15) ? 1 : 0) - (pressed(gp, 14) ? 1 : 0);
    const dpadY = (pressed(gp, 12) ? 1 : 0) - (pressed(gp, 13) ? 1 : 0);
    return {
      id: gp.id,
      move: { x: axis(gp.axes[0]) || dpadX, y: -axis(gp.axes[1]) || dpadY },
      look: { x: axis(gp.axes[2]), y: -axis(gp.axes[3]) },
      turnLeft: pressed(gp, 4), turnRight: pressed(gp, 5),
      // Face buttons by position: top climbs, bottom descends, right is fast.
      // On the iBuffalo (verified 2026-10-04) that is X, B and A; on an Xbox
      // pad Y, A and B. Triggers also climb/descend, but a pad whose shoulder
      // reports its trigger too (the iBuffalo: L = 4+6, R = 5+7) must not
      // read a turn as a descent, so a trigger counts only without its shoulder.
      up: pressed(gp, 3) || (pressed(gp, 7) && !pressed(gp, 5)),
      down: pressed(gp, 0) || (pressed(gp, 6) && !pressed(gp, 4)),
      fast: pressed(gp, 1),
      start: start && !prev.start, select: select && !prev.select,
    };
  }
  // SNES-shaped: A(0) B(1) X(2) Y(3) L(4) R(5) Select(6) Start(7); d-pad as axes.
  return {
    id: gp.id,
    move: { x: Math.sign(axis(gp.axes[0])), y: -Math.sign(axis(gp.axes[1])) },
    look: { x: 0, y: 0 },
    turnLeft: pressed(gp, 4), turnRight: pressed(gp, 5),
    up: pressed(gp, 2), down: pressed(gp, 1),
    fast: pressed(gp, 0),
    start: start && !prev.start, select: select && !prev.select,
  };
}

export class GamepadReader {
  private raf = 0;
  private prev: Held = { start: false, select: false };
  private lastId: string | null = null;

  /** `onInput` runs once per animation frame while a pad is present; `onPresence` on connect/disconnect with the pad's id. */
  constructor(private onInput: (p: PadInput, gp: Gamepad) => void, private onPresence: (id: string | null) => void) {
    window.addEventListener('gamepadconnected', () => this.start());
    window.addEventListener('gamepaddisconnected', () => this.start());
    this.start();
  }

  private start(): void {
    cancelAnimationFrame(this.raf);
    const tick = () => {
      const gp = Array.from(navigator.getGamepads?.() ?? []).find((g): g is Gamepad => !!g && g.connected);
      const id = gp ? gp.id : null;
      if (id !== this.lastId) { this.lastId = id; this.onPresence(id); }
      if (gp) {
        const input = read(gp, this.prev);
        this.prev = held(gp);
        this.onInput(input, gp);
      } else {
        this.prev = { start: false, select: false };
      }
      this.raf = requestAnimationFrame(tick);
    };
    this.raf = requestAnimationFrame(tick);
  }
}
