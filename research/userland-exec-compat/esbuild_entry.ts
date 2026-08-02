import { label, square } from './esbuild_math';

const values: number[] = Array.from({ length: 128 }, (_, index) => square(index));
console.log(label, values.at(-1), globalThis?.Object?.keys({ traced: true }).length);
