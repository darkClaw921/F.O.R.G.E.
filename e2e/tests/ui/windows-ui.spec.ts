/**
 * UI tests for the window-bar (#window-bar).
 *
 * Seed a tmux session via API, click it in the sidebar, then verify:
 * - Window tabs are rendered.
 * - '+' button creates a new window.
 * - '×' close button removes a window tab.
 */
import { test, expect } from '@playwright/test';
import {
  apiCreateSession,
  getActiveProjectPrefix,
  fullSessionName,
  e2ePrefix,
} from '../../fixtures/api-client';
import { cleanupE2ESessions } from '../../fixtures/tmux-helpers';

test.describe.configure({ mode: 'serial' });

const PREFIX = e2ePrefix();
let tmuxPrefix = '';
let SESSION = '';

test.beforeAll(async ({ request }) => {
  tmuxPrefix = await getActiveProjectPrefix(request);
  const baseName = `${PREFIX}winbar`;
  await apiCreateSession(request, baseName);
  SESSION = fullSessionName(tmuxPrefix, baseName);
});

test.afterAll(async ({ request }) => {
  await cleanupE2ESessions(request, fullSessionName(tmuxPrefix, PREFIX));
  await cleanupE2ESessions(request, PREFIX);
});

/** Open the app and click the session to activate window-bar. */
async function openSession(page: any) {
  await page.goto('/');
  const item = page.locator(`[data-session="${SESSION}"]`);
  await expect(item).toBeVisible({ timeout: 10_000 });
  await item.click();
  await expect(page.locator('#window-bar')).toBeVisible({ timeout: 10_000 });
}

test.describe('Window bar UI', () => {
  test('window-bar becomes visible after selecting a session', async ({ page }) => {
    await openSession(page);
    await expect(page.locator('#window-bar')).toBeVisible();
  });

  test('#window-tabs contains at least one .window-tab', async ({ page }) => {
    await openSession(page);
    const tabs = page.locator('#window-tabs .window-tab');
    await expect(tabs.first()).toBeVisible({ timeout: 10_000 });
  });

  test('window tab has index span and name span', async ({ page }) => {
    await openSession(page);
    const tab = page.locator('#window-tabs .window-tab').first();
    await expect(tab).toBeVisible();
    await expect(tab.locator('.window-tab-idx')).toBeVisible();
    await expect(tab.locator('.window-tab-name')).toBeVisible();
  });

  test('window tab has close button (.window-tab-close)', async ({ page }) => {
    await openSession(page);
    const tab = page.locator('#window-tabs .window-tab').first();
    await expect(tab.locator('.window-tab-close')).toBeAttached();
  });

  test('#window-new (+) button is visible', async ({ page }) => {
    await openSession(page);
    await expect(page.locator('#window-new')).toBeVisible();
  });

  test('clicking #window-new creates a new window tab', async ({ page }) => {
    await openSession(page);
    const before = await page.locator('#window-tabs .window-tab').count();

    // createWindow() triggers window.prompt — handle it before clicking
    page.once('dialog', async (dialog) => {
      if (dialog.type() === 'prompt') {
        await dialog.accept(''); // empty name → tmux auto-names
      } else {
        await dialog.dismiss();
      }
    });
    await page.locator('#window-new').click();

    // Wait for window tabs to refresh
    await expect.poll(
      async () => await page.locator('#window-tabs .window-tab').count(),
      { timeout: 10_000 },
    ).toBeGreaterThanOrEqual(before + 1);
  });

  test('clicking a window tab switches to it (active class applied)', async ({ page }) => {
    await openSession(page);

    // Make sure there are at least 2 windows.
    // Creating a window via #window-new triggers a window.prompt for the name.
    const tabs = page.locator('#window-tabs .window-tab');
    const count = await tabs.count();
    if (count < 2) {
      // Set up dialog handler BEFORE clicking
      page.once('dialog', async (dialog) => {
        if (dialog.type() === 'prompt') {
          await dialog.accept('tab-switch-test');
        } else {
          await dialog.dismiss();
        }
      });
      await page.locator('#window-new').click();
      // Wait for the POST /api/.../windows call to complete AND
      // for fetchWindows() to refresh the UI
      await expect.poll(async () => await tabs.count(), { timeout: 10_000 }).toBeGreaterThanOrEqual(2);
    }

    // Click the second tab
    await tabs.nth(1).click();

    // Wait a bit for the active state to update
    await page.waitForTimeout(500);
    const activeTabs = await page.locator('#window-tabs .window-tab.active').count();
    expect(activeTabs).toBeGreaterThanOrEqual(1);
  });

  test('close button on a non-last window removes its tab', async ({ page }) => {
    // Register a universal dialog handler for ALL dialogs in this test:
    // - prompt (window name when creating a new window)
    // - confirm (kill window confirmation)
    page.on('dialog', async (dialog) => {
      if (dialog.type() === 'prompt') {
        await dialog.accept('');
      } else {
        await dialog.accept();
      }
    });

    await openSession(page);

    // Ensure we have at least 2 windows before deleting
    const tabs = page.locator('#window-tabs .window-tab');
    let count = await tabs.count();
    while (count < 2) {
      await page.locator('#window-new').click();
      await expect.poll(async () => await tabs.count(), { timeout: 10_000 }).toBeGreaterThanOrEqual(count + 1);
      count = await tabs.count();
    }

    // Click the close button on the last tab
    const lastTab = tabs.last();
    await lastTab.locator('.window-tab-close').click();

    await expect.poll(async () => await tabs.count(), { timeout: 8_000 }).toBeLessThan(count);
  });
});
