// Ethan, 2026-10-04, on a phone with the server unreachable: the Files tab sat
// on "Loading..." for a folder it had shown before ("this should load offline
// cached locally"), a file marked saved-offline did the same in the preview,
// and the preview filled only the top half of the screen ("this view is a
// bug"). A hung request never reached the cache, which was only read after a
// FAILURE; and a keyboard-time height outlived the keyboard on a zoomed page.
import { mkdtemp, rm, writeFile, mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test, expect } from './fixtures';
import { cleanup } from './teardown';

test('Files shows the saved listing and file at once when the server hangs, and the preview fills the screen', async ({ page, request }, info) => {
  test.setTimeout(90_000);
  const dir = await mkdtemp(join(tmpdir(), 'amux-files-cache-'));
  await mkdir(join(dir, 'essays'));
  await writeFile(join(dir, 'essays', 'self-reliance.md'), '# Self-Reliance\n\nTrust thyself.\n');
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.goto('/');
  const headers = await page.evaluate(() => ({ Authorization: 'Bearer ' + (window as any)._AMUX_AUTH_TOKEN }));
  const prior = await (await request.get('/api/prefs?key=files_cwd', { headers })).json();
  try {
    expect((await request.post('/api/prefs', { headers, data: { key: 'files_cwd', value: dir } })).ok()).toBeTruthy();
    await page.reload();
    await page.waitForFunction(() => typeof (window as any).loadFiles === 'function');
    await page.locator('#tab-files').click();
    await expect(page.locator('#files-body')).toBeVisible();
    // Online once: the listing and the file are saved on the device.
    const folder = join(dir, 'essays');
    await page.evaluate(p => (window as any).loadFiles(p), folder);
    await expect(page.locator('#files-body .fe-row').filter({ hasText: 'self-reliance.md' })).toBeVisible();
    const file = join(folder, 'self-reliance.md');
    await page.evaluate(p => (window as any).openFilePreview(p), file);
    await expect(page.locator('#file-body')).toContainText('Trust thyself');
    await page.evaluate(() => (window as any).closeFilePreview());
    // Give the IndexedDB writes a moment to land.
    await page.waitForTimeout(500);

    // Now the server stops answering: requests hang rather than fail.
    await page.route('**/api/ls**', () => {});
    await page.route('**/api/file?**', () => {});
    // Fire and forget: with the server hanging, the load never settles.
    await page.evaluate(() => { (window as any).loadFiles('/'); });
    await page.evaluate(p => { (window as any).loadFiles(p); }, folder);
    await expect(page.locator('#files-body .fe-row').filter({ hasText: 'self-reliance.md' }),
      'the saved listing shows while the request hangs').toBeVisible({ timeout: 3000 });
    await expect(page.locator('#files-body')).toContainText('Offline cache');

    await page.evaluate(p => { (window as any).openFilePreview(p); }, file);
    await expect(page.locator('#file-body'), 'the saved file shows while the request hangs')
      .toContainText('Trust thyself', { timeout: 3000 });

    // A height left over from a keyboard (or a zoomed page) must not hold the
    // preview at half the screen once no field has focus.
    const fill = await page.evaluate(async () => {
      document.documentElement.style.setProperty('--dialog-viewport-height', '300px');
      (document.activeElement as HTMLElement | null)?.blur?.();
      window.dispatchEvent(new Event('resize'));
      await new Promise(r => setTimeout(r, 100));
      const ov = document.getElementById('file-overlay')!.getBoundingClientRect();
      return { bottom: ov.bottom, inner: innerHeight };
    });
    expect(fill.bottom, JSON.stringify(fill)).toBeGreaterThan(fill.inner - 2);
  } finally {
    await cleanup('restore files_cwd', () => request.post('/api/prefs', { headers, data: { key: 'files_cwd', value: prior.value || '' } }), info);
    await cleanup('remove temp folder', () => rm(dir, { recursive: true, force: true }), info);
  }
});
