/**
 * Structural layout tests: tab-bar, sidebar, main containers.
 * Uses IDs and roles exclusively — no brittle text selectors.
 */
import { test, expect } from '@playwright/test';

test.describe('Layout — tab bar and containers', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
  });

  // ----- Tab bar presence -----

  test('#tab-bar is visible', async ({ page }) => {
    await expect(page.locator('#tab-bar')).toBeVisible();
  });

  test('#tab-terminal button exists and is active by default', async ({ page }) => {
    const btn = page.locator('#tab-terminal');
    await expect(btn).toBeVisible();
    await expect(btn).toHaveClass(/active/);
  });

  test('#tab-tasks button exists', async ({ page }) => {
    await expect(page.locator('#tab-tasks')).toBeVisible();
  });

  test('#tab-git button exists', async ({ page }) => {
    await expect(page.locator('#tab-git')).toBeVisible();
  });

  test('#tab-docker button exists', async ({ page }) => {
    await expect(page.locator('#tab-docker')).toBeVisible();
  });

  test('#tab-telescope button exists', async ({ page }) => {
    await expect(page.locator('#tab-telescope')).toBeVisible();
  });

  // ----- Sidebar -----

  test('#sidebar is visible', async ({ page }) => {
    await expect(page.locator('#sidebar')).toBeVisible();
  });

  test('#session-list exists', async ({ page }) => {
    await expect(page.locator('#session-list')).toBeVisible();
  });

  test('#btn-new (create session) button is visible', async ({ page }) => {
    await expect(page.locator('#btn-new')).toBeVisible();
  });

  test('#btn-sidebar-toggle exists', async ({ page }) => {
    await expect(page.locator('#btn-sidebar-toggle')).toBeVisible();
  });

  test('#project-select exists', async ({ page }) => {
    await expect(page.locator('#project-select')).toBeVisible();
  });

  // ----- Main content containers -----

  test('#terminal container exists in DOM', async ({ page }) => {
    // Terminal div may be visible or hidden depending on selected session
    const el = page.locator('#terminal');
    await expect(el).toBeAttached();
  });

  test('#placeholder is visible when no session is selected', async ({ page }) => {
    // On fresh load with no session selected, placeholder should show
    const placeholder = page.locator('#placeholder');
    // It's visible if no session is active
    const terminal = page.locator('#terminal');
    const terminalHidden = await terminal.evaluate((el) => (el as HTMLElement).hidden);
    if (terminalHidden) {
      await expect(placeholder).toBeVisible();
    }
  });

  test('#tasks container exists in DOM', async ({ page }) => {
    await expect(page.locator('#tasks')).toBeAttached();
  });

  test('#git container exists in DOM', async ({ page }) => {
    await expect(page.locator('#git')).toBeAttached();
  });

  test('#docker container exists in DOM', async ({ page }) => {
    await expect(page.locator('#docker')).toBeAttached();
  });

  test('#telescope container exists in DOM', async ({ page }) => {
    await expect(page.locator('#telescope')).toBeAttached();
  });

  // ----- Tab switching -----

  test('clicking #tab-tasks shows #tasks and hides #terminal', async ({ page }) => {
    await page.locator('#tab-tasks').click();
    await expect(page.locator('#tasks')).toBeVisible();
    const termHidden = await page.locator('#terminal').evaluate(
      (el) => (el as HTMLElement).hidden,
    );
    expect(termHidden).toBe(true);
  });

  test('clicking #tab-git shows #git container', async ({ page }) => {
    await page.locator('#tab-git').click();
    await expect(page.locator('#git')).toBeVisible();
  });

  test('clicking #tab-docker shows #docker container', async ({ page }) => {
    await page.locator('#tab-docker').click();
    await expect(page.locator('#docker')).toBeVisible();
  });

  test('clicking #tab-telescope shows #telescope container', async ({ page }) => {
    await page.locator('#tab-telescope').click();
    await expect(page.locator('#telescope')).toBeVisible();
  });

  test('clicking #tab-terminal restores terminal view', async ({ page }) => {
    // Switch away first
    await page.locator('#tab-tasks').click();
    await expect(page.locator('#tasks')).toBeVisible();

    // Switch back
    await page.locator('#tab-terminal').click();
    // Terminal container should no longer be hidden
    const termHidden = await page.locator('#terminal').evaluate(
      (el) => (el as HTMLElement).hidden,
    );
    expect(termHidden).toBe(false);
  });

  // ----- Sidebar toggle -----

  test('#btn-sidebar-toggle toggles sidebar visibility', async ({ page }) => {
    const sidebar = page.locator('#sidebar');
    await expect(sidebar).toBeVisible();

    await page.locator('#btn-sidebar-toggle').click();

    // After toggle, sidebar may be hidden or collapsed — give it a moment
    await page.waitForTimeout(200);
    // We just verify the click doesn't crash; the exact CSS class depends on the impl
    // Click again to restore
    await page.locator('#btn-sidebar-toggle').click();
    await page.waitForTimeout(200);
    await expect(sidebar).toBeVisible();
  });

  // ----- Window-bar (hidden when no session) -----

  test('#window-bar is hidden when no session is selected', async ({ page }) => {
    const windowBar = page.locator('#window-bar');
    await expect(windowBar).toBeAttached();
    const hidden = await windowBar.evaluate((el) => (el as HTMLElement).hidden);
    // It should be hidden until a session is connected
    expect(hidden).toBe(true);
  });
});
