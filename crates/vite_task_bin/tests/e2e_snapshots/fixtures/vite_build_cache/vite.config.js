import { defineConfig } from 'vite';

export default defineConfig({
  logLevel: 'silent',
  plugins: [
    {
      name: 'assert-node-env',
      config() {
        const expectedNodeEnv = process.env.ASSERT_NODE_ENV;
        if (expectedNodeEnv !== undefined && process.env.NODE_ENV !== expectedNodeEnv) {
          throw new Error(
            `Expected NODE_ENV to be ${JSON.stringify(expectedNodeEnv)}, received ${JSON.stringify(process.env.NODE_ENV)}`,
          );
        }
      },
    },
  ],
  build: {
    rollupOptions: {
      output: {
        // Stable filenames make cache behaviour deterministic across runs.
        entryFileNames: 'assets/main.js',
        chunkFileNames: 'assets/chunk.js',
        assetFileNames: 'assets/[name][extname]',
      },
    },
  },
});
