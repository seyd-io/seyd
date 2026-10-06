// Copyright 2026 Anton Gravestam
// SPDX-License-Identifier: Apache-2.0
import { defineConfig } from 'vitest/config';
export default defineConfig({ test: { include: ['test/**/*.test.ts'], testTimeout: 30000 } });
