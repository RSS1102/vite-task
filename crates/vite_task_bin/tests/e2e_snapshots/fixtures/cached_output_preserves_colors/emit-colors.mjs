#!/usr/bin/env node
// Mimics how vitest (via tinyrainbow / picocolors) decides whether to emit
// ANSI color codes: NO_COLOR disables it, FORCE_COLOR (when truthy) enables
// it, otherwise it falls back to `process.stdout.isTTY`. This stand-in lets
// us reproduce the issue without pulling vitest's full install into the e2e
// harness.
const noColor = "NO_COLOR" in process.env && process.env.NO_COLOR !== "";
const forceColor = process.env.FORCE_COLOR;
const forceColorTruthy =
  forceColor !== undefined && forceColor !== "" && forceColor !== "0";
const useColor = !noColor && (forceColorTruthy || Boolean(process.stdout.isTTY));

const green = (s) => (useColor ? `\x1b[32m${s}\x1b[0m` : s);
const red = (s) => (useColor ? `\x1b[31m${s}\x1b[0m` : s);
const dim = (s) => (useColor ? `\x1b[2m${s}\x1b[0m` : s);
const bold = (s) => (useColor ? `\x1b[1m${s}\x1b[0m` : s);

process.stdout.write(`${green("✓")} ${dim("example.test.ts")}\n`);
process.stdout.write(`${green("✓")} ${dim("another.test.ts")}\n`);
process.stdout.write(
  `\nTest Files  ${bold(green("2 passed"))} (2)\n     Tests  ${bold(green("4 passed"))} (4)\n`,
);
