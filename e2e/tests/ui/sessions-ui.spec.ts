/**
 * UI tests for session management:
 * - Sessions seeded via API appear in the sidebar.
 * - Kill button removes the session.
 * - Clicking session opens it (window-bar becomes visible).
 * - +new button creates a session.
 */
import { test, expect } from '@playwright/test';
import {
  apiCreateSession,
  apiDeleteSession,
  apiListSessions,
  getActiveProjectPrefix,
  fullSessionName,
  e2ePrefix,
} from '../../fixtures/api-client';
import { cleanupE2ESessions } from '../../fixtures/tmux-helpers';

test.describe.configure({ mode: 'serial' });

const PREFIX = e2ePrefix();
let tmuxPrefix = '';

test.beforeAll(async ({ request }) => {
  tmuxPrefix = await getActiveProjectPrefix(request);
});

test.afterAll(async ({ request }) => {
  await cleanupE2ESessions(request, fullSessionName(tmuxPrefix, PREFIX));
  await cleanupE2ESessions(request, PREFIX);
});

test.describe('Sessions UI', () => {
  test('session created via API appears in sidebar', async ({ page, request }) => {
    const baseName = `${PREFIX}ui-appear`;
    await apiCreateSession(request, baseName);
    const fullName = fullSessionName(tmuxPrefix, baseName);

    await page.goto('/');

    // Poll until sidebar refreshes (app polls on an interval)
    await expect(
      page.locator(`[data-session="${fullName}"]`),
    ).toBeVisible({ timeout: 10_000 });

    // Cleanup
    await apiDeleteSession(request, fullName);
  });

  test('session item has .session-name and .session-sub children', async ({ page, request }) => {
    const baseName = `${PREFIX}ui-shape`;
    await apiCreateSession(request, baseName);
    const fullName = fullSessionName(tmuxPrefix, baseName);

    await page.goto('/');
    const item = page.locator(`[data-session="${fullName}"]`);
    await expect(item).toBeVisible({ timeout: 10_000 });

    await expect(item.locator('.session-name')).toBeVisible();
    await expect(item.locator('.session-sub')).toBeVisible();

    await apiDeleteSession(request, fullName);
  });

  test('session item has rename and kill buttons', async ({ page, request }) => {
    const baseName = `${PREFIX}ui-btns`;
    await apiCreateSession(request, baseName);
    const fullName = fullSessionName(tmuxPrefix, baseName);

    await page.goto('/');
    const item = page.locator(`[data-session="${fullName}"]`);
    await expect(item).toBeVisible({ timeout: 10_000 });

    await expect(item.locator('.btn-rename')).toBeAttached();
    await expect(item.locator('.btn-kill')).toBeAttached();

    await apiDeleteSession(request, fullName);
  });

  test('kill button removes session from sidebar', async ({ page, request }) => {
    const baseName = `${PREFIX}ui-kill`;
    await apiCreateSession(request, baseName);
    const fullName = fullSessionName(tmuxPrefix, baseName);

    await page.goto('/');
    const item = page.locator(`[data-session="${fullName}"]`);
    await expect(item).toBeVisible({ timeout: 10_000 });

    // Handle window.confirm dialog
    page.on('dialog', (dialog) => dialog.accept());

    await item.locator('.btn-kill').click();

    // Session should disappear from the list
    await expect(page.locator(`[data-session="${fullName}"]`)).toBeHidden({ timeout: 10_000 });

    // Verify via API it's actually gone
    const sessions = await apiListSessions(request);
    expect(sessions.find((s) => s.name === fullName)).toBeUndefined();
  });

  test('clicking session item connects to it (window-bar becomes visible)', async ({
    page,
    request,
  }) => {
    const baseName = `${PREFIX}ui-connect`;
    await apiCreateSession(request, baseName);
    const fullName = fullSessionName(tmuxPrefix, baseName);

    await page.goto('/');
    const item = page.locator(`[data-session="${fullName}"]`);
    await expect(item).toBeVisible({ timeout: 10_000 });

    // Click to open the session
    await item.click();

    // After selecting the session the window-bar should become visible
    await expect(page.locator('#window-bar')).toBeVisible({ timeout: 10_000 });

    await apiDeleteSession(request, fullName);
  });

  test('+new button shows create-session dialog / prompt', async ({ page }) => {
    await page.goto('/');

    // Handle window.prompt — fill in a session name, confirm
    const created: string[] = [];
    page.on('dialog', async (dialog) => {
      if (dialog.type() === 'prompt') {
        const name = `${PREFIX}ui-create-dlg`;
        created.push(name);
        await dialog.accept(name);
      } else if (dialog.type() === 'alert') {
        // In case of an alert error, dismiss and let the test fail naturally
        await dialog.dismiss();
      } else {
        await dialog.dismiss();
      }
    });

    await page.locator('#btn-new').click();

    // After creation the new session should appear in the list
    // The session name will have the project prefix prepended
    await expect(
      page.locator('#session-list').locator('[data-session]').last(),
    ).toBeVisible({ timeout: 10_000 });
  });
});
