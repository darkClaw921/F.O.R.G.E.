/**
 * UI tests for theme switching.
 *
 * We use the API to switch themes and verify the CSS variables on :root
 * change accordingly. We avoid relying on the full theme-editor modal UI
 * (which would be fragile) and instead verify the end state after API calls.
 */
import { test, expect } from '@playwright/test';
import {
  apiGetActiveTheme,
  apiListThemes,
  apiPatchActiveTheme,
} from '../../fixtures/api-client';

test.describe('Themes UI', () => {
  let originalActiveId: string;

  test.beforeAll(async ({ request }) => {
    const active = await apiGetActiveTheme(request);
    originalActiveId = active.id;
  });

  test.afterAll(async ({ request }) => {
    // Restore original theme
    try {
      await apiPatchActiveTheme(request, originalActiveId);
    } catch { /* best effort */ }
  });

  test('page loads with a theme applied (CSS variables present on :root)', async ({ page }) => {
    await page.goto('/');
    // The app sets CSS variables from the active theme. Check at least one.
    const bg = await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue('--bg').trim(),
    );
    // bg should be a hex colour string set by the theme
    expect(bg.length).toBeGreaterThan(0);
  });

  test('switching theme via API changes CSS variables in the browser after reload', async ({
    page,
    request,
  }) => {
    const list = await apiListThemes(request);
    // Pick a preset different from the current active
    const other = list.presets.find((p: any) => p.id !== list.active);
    if (!other) {
      // Only one preset — can't test diff; still a pass
      return;
    }

    // Switch theme via API
    await apiPatchActiveTheme(request, other.id);

    // Reload the page so JS picks up the new theme from GET /api/themes/active
    await page.goto('/');

    // Wait for JS to apply theme
    await page.waitForFunction(
      () => {
        const bg = getComputedStyle(document.documentElement).getPropertyValue('--bg');
        return bg.trim().length > 0;
      },
      { timeout: 8_000 },
    );

    const bg = await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue('--bg').trim(),
    );
    expect(bg).toBe(other.ui.bg);
  });

  test('GET /api/themes/active colour fields are applied to :root on page load', async ({
    page,
    request,
  }) => {
    await page.goto('/');
    const theme = await apiGetActiveTheme(request);

    // Wait for theme to be applied
    await page.waitForFunction(
      (expectedBg) => {
        const bg = getComputedStyle(document.documentElement).getPropertyValue('--bg');
        return bg.trim() === expectedBg;
      },
      theme.ui.bg,
      { timeout: 8_000 },
    );

    const actualBg = await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue('--bg').trim(),
    );
    expect(actualBg).toBe(theme.ui.bg);
  });
});
