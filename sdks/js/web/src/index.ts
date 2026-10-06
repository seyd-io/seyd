// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
/**
 * Framework-agnostic custom elements over `@seyd/core`: `<seyd-video>`, `<seyd-hud>` and `<seyd-connect-error>`, plus `classify()`, which maps a failed connection to its networking guidance.
 *
 * @module @seyd/web
 */
export { SeydVideoElement } from './seyd-video.js';
export { SeydHudElement } from './seyd-hud.js';
export { SeydConnectErrorElement, classify } from './seyd-connect-error.js';
export type { FailureClass, Guidance } from './seyd-connect-error.js';
