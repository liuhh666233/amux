// Worker Messages tab: Ask and Export fold into an ellipsis menu on a phone
// (Ethan, 2026-10-01: "make ask and export ellipse on mobile").
import { test, expect } from './fixtures';

test('Messages tab: Ask and Export sit in the row on wide screens and in a ⋯ menu on a phone', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._peekAskToggle === 'function');
  // The panel is static markup; show it without opening a real worker.
  await page.evaluate(() => {
    let el: HTMLElement | null = document.getElementById('peek-messages-panel');
    (el as HTMLElement).style.display = 'flex';
    (el as HTMLElement).style.pointerEvents = 'auto';
    el = (el as HTMLElement).parentElement;
    while (el) {
      el.hidden = false; el.removeAttribute('inert'); el.removeAttribute('aria-hidden');
      if (getComputedStyle(el).display === 'none') el.style.display = 'block';
      if (getComputedStyle(el).visibility === 'hidden') el.style.visibility = 'visible';
      // A closed overlay ignores taps; the real peek is open when this row is used.
      if (getComputedStyle(el).pointerEvents === 'none') el.style.pointerEvents = 'auto';
      el = el.parentElement;
    }
  });
  const phone = (page.viewportSize()?.width || 1200) <= 600;
  const wide = page.locator('#peek-messages-panel .pm-wide');
  const more = page.locator('#peek-msgs-more');
  if (phone) {
    await expect(more).toBeVisible();
    await expect(wide.first()).toBeHidden();
    await expect(page.locator('#peek-msgs-date-jump')).toBeHidden();
    const search = await page.locator('#peek-messages-search').boundingBox();
    expect(search!.width).toBeGreaterThanOrEqual(150);
    await expect(more.locator('.pm-date-item input[type=date]')).toHaveCount(1);
    const box = await more.locator('summary').boundingBox();
    expect(box!.height).toBeGreaterThanOrEqual(44);
    await more.locator('summary').click();
    await more.getByText('Ask about these messages').click();
    await expect(page.locator('#peek-ask-panel')).toBeVisible();
    await expect(more).not.toHaveAttribute('open', '');
  } else {
    await expect(wide.first()).toBeVisible();
    await expect(more).toBeHidden();
  }
});
