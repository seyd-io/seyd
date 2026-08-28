import { defineConfig } from 'vite';
import { resolve } from 'node:path';
export default defineConfig({
  build: {
    target: 'es2022',
    rollupOptions: { input: { landing: resolve(__dirname, 'index.html'), pilot: resolve(__dirname, 'pilot/index.html') } },
  },
  worker: { format: 'es' },
  server: { port: 3000 },
});
