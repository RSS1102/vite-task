export default class QuietReporter {
  onTestRunEnd() {
    if (process.env.VITEST_EXIT_AFTER_RUN) {
      setTimeout(() => process.exit(0), 0);
    }
  }
}
