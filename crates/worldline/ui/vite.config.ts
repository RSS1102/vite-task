import vue from '@vitejs/plugin-vue';
import { defineConfig } from 'vite';

// Relative base so the embedded server can serve assets from any path, and a
// fixed asset layout so the embedded-asset table and index.html stay in sync.
export default defineConfig({
  plugins: [vue()],
  base: './',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    chunkSizeWarningLimit: 4096,
  },
});
