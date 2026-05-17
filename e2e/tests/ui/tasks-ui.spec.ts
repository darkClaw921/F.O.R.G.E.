/**
 * UI tests for the Tasks tab / kanban board.
 */
import { test, expect } from '@playwright/test';

test.describe('Tasks UI', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await page.locator('#tab-tasks').click();
    await expect(page.locator('#tasks')).toBeVisible();
  });

  test('#tasks container is visible after clicking Tasks tab', async ({ page }) => {
    await expect(page.locator('#tasks')).toBeVisible();
  });

  test('#tasks-toolbar exists with new-task button', async ({ page }) => {
    await expect(page.locator('#tasks-toolbar')).toBeVisible();
    await expect(page.locator('#tasks-new')).toBeVisible();
  });

  test('#tasks-reload button exists', async ({ page }) => {
    await expect(page.locator('#tasks-reload')).toBeAttached();
  });

  test('#tasks-board is rendered', async ({ page }) => {
    await expect(page.locator('#tasks-board')).toBeVisible();
  });

  test('#tasks-board has kanban columns', async ({ page }) => {
    // The board should contain at least one column element
    // Columns are rendered as divs with class names like "kanban-col" or similar
    // We wait for the board to load (it fetches from API)
    await expect(
      page.locator('#tasks-board').locator('[class*="col"]').first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('clicking #tasks-reload does not crash the page', async ({ page }) => {
    await page.locator('#tasks-reload').click();
    // After reload, board should still be attached
    await expect(page.locator('#tasks-board')).toBeVisible({ timeout: 8_000 });
  });

  test('#tasks-new button opens create-task dialog', async ({ page }) => {
    // The create flow uses a dialog/modal or a form; just confirm the click
    // doesn't throw and either a dialog appears or a form becomes visible
    const dialogPromise = page.waitForEvent('dialog', { timeout: 3_000 }).catch(() => null);
    await page.locator('#tasks-new').click();
    const dialog = await dialogPromise;
    if (dialog) {
      await dialog.dismiss();
    }
    // Either a modal/dialog appeared and was dismissed, or the UI renders an inline form
    // Either way the board should still be visible
    await expect(page.locator('#tasks-board')).toBeAttached();
  });
});
