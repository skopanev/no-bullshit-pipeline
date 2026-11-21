import { expect, test } from 'bun:test';

test('startup styles and scripts do not fetch remote resources', async () => {
  const css = await Bun.file(new URL('../src/styles.css', import.meta.url)).text();
  const html = await Bun.file(new URL('../src/index.html', import.meta.url)).text();
  expect(css).not.toMatch(/@import\s+(?:url\()?\s*["']?https?:/i);
  expect(css).not.toMatch(/url\(\s*["']?https?:/i);
  expect(html).not.toMatch(/<(?:script|link)\b[^>]*(?:src|href)=["']https?:/i);
});

test('bootstrap waits only for the DOM, not network-dependent window load', async () => {
  const main = await Bun.file(new URL('../src/js/main.js', import.meta.url)).text();
  expect(main).toContain("document.addEventListener('DOMContentLoaded', bootstrapApp");
  expect(main).not.toMatch(/addEventListener\(['"]load['"],\s*bootstrapApp/);
});
