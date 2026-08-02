'use strict';

const fs = require('node:fs');

const [esbuildModule, physicalWrapper, logicalBinary, entryPoint] = process.argv.slice(2);
process.env.ESBUILD_BINARY_PATH = physicalWrapper;
process.env.LIBREFLECT_TARGET = logicalBinary;

const esbuild = require(esbuildModule);

async function main() {
  const built = await esbuild.build({
    bundle: true,
    entryPoints: [entryPoint],
    format: 'esm',
    minify: true,
    sourcemap: 'inline',
    write: false,
  });
  const transformed = await esbuild.transform('const answer: number = 6 * 7', {
    loader: 'ts',
    minify: true,
  });
  console.log(
    JSON.stringify({
      esbuild: esbuild.version,
      outputFiles: built.outputFiles.map((file) => ({
        path: file.path,
        bytes: file.contents.length,
      })),
      transformed: transformed.code.trim(),
      entryBytes: fs.statSync(entryPoint).size,
    }),
  );
}

main().catch((error) => {
  console.error(error.stack || error);
  process.exitCode = 1;
});
