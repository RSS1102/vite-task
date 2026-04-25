import { expect, test } from 'vitest';

test('addition', () => {
  expect(1 + 1).toBe(2);
});

test('string concat', () => {
  expect(`hello ${'world'}`).toBe('hello world');
});
