import { playwright } from '@vitest/browser-playwright';
import { defineConfig } from 'vitest/config';
import { DefaultReporter } from 'vitest/node';

class NoTestSummaryReporter extends DefaultReporter {
  reportTestSummary() {}
}

const chromiumSandbox = process.env.VITEST_CHROMIUM_SANDBOX === 'true';

export default defineConfig({
  test: {
    reporters: [new NoTestSummaryReporter({ summary: false }), 'json'],
    outputFile: { json: 'dist/result.json' },
    browser: {
      enabled: true,
      headless: true,
      provider: playwright({
        launchOptions: {
          chromiumSandbox,
          args: chromiumSandbox ? ['--enable-logging=stderr', '--v=1'] : [],
        },
      }),
      instances: [{ browser: 'chromium' }],
    },
  },
});
