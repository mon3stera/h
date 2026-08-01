import { resolve } from 'node:path';
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// The webview bundle is loaded through vscode-webview URIs, so asset URLs must
// stay relative; the extension injects a <base> tag pointing at the dist root.
export default defineConfig({
  plugins: [react()],
  root: 'src/webview',
  base: './',
  build: {
    outDir: resolve(process.cwd(), 'dist/webview'),
    emptyOutDir: true,
    rollupOptions: {
      input: 'index.html',
    },
  },
});
