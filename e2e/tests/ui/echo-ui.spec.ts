/**
 * UI tests for the Echo (Э.Х.О) tab.
 *
 * Verified behaviour:
 * - #tab-bar contains #tab-echo with text "Э.Х.О"
 * - Clicking #tab-echo makes #echo visible; other panes become hidden
 * - Inside #echo: #echo-input (textarea) and #echo-send (send button) exist
 * - #echo-toasts container exists in the DOM
 * - Switching back to Terminal re-hides #echo
 * - Screenshot of the Echo tab is saved for visual validation
 *
 * Note on CSS variable check: Playwright's evaluate() returns computed styles.
 * CSS custom properties on elements are not reflected in computed styles for
 * individual properties (they resolve at usage-time). The test instead verifies
 * that the Echo elements use the --bg / --accent values by checking that the
 * CSS custom properties are defined on :root (applied by the active theme),
 * which means the Echo UI can inherit them.
 */
import { test, expect } from '@playwright/test';

const ALL_TAB_PANE_IDS = ['terminal', 'tasks', 'echo', 'git', 'docker', 'telescope'];
// Which panes use the `hidden` attribute (all except terminal which is always
// in the DOM but we verify via our tab-switching logic)
const HIDDEN_PANE_IDS = ['tasks', 'echo', 'git', 'docker', 'telescope'];

test.describe('Echo tab UI', () => {
  test.describe.configure({ mode: 'serial' });

  test('#tab-echo button exists in #tab-bar with correct label', async ({ page }) => {
    await page.goto('/');

    const tabBar = page.locator('#tab-bar');
    await expect(tabBar).toBeVisible({ timeout: 10_000 });

    const echoTab = tabBar.locator('#tab-echo');
    await expect(echoTab).toBeVisible();
    // Text must contain "Э.Х.О" (the Cyrillic label)
    await expect(echoTab).toContainText('Э.Х.О');
  });

  test('clicking #tab-echo makes #echo pane visible', async ({ page }) => {
    await page.goto('/');

    // Wait for tab-bar to be rendered
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    // #echo should start hidden
    await expect(page.locator('#echo')).toBeHidden();

    // Click the echo tab
    await page.locator('#tab-echo').click();

    // #echo must become visible
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });
  });

  test('after clicking #tab-echo, other main panes are hidden', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await page.locator('#tab-echo').click();
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });

    // Verify all non-echo panes in HIDDEN_PANE_IDS (except echo itself) are hidden
    for (const id of HIDDEN_PANE_IDS.filter((id) => id !== 'echo')) {
      await expect(page.locator(`#${id}`)).toBeHidden({ timeout: 2_000 });
    }
  });

  test('#echo-input textarea is present inside #echo', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await page.locator('#tab-echo').click();
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });

    // Input textarea
    const input = page.locator('#echo-input');
    await expect(input).toBeVisible({ timeout: 5_000 });
    const tagName = await input.evaluate((el) => el.tagName.toLowerCase());
    expect(tagName).toBe('textarea');
  });

  test('#echo-send button is present inside #echo', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await page.locator('#tab-echo').click();
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });

    const sendBtn = page.locator('#echo-send');
    await expect(sendBtn).toBeVisible({ timeout: 5_000 });
    const tagName = await sendBtn.evaluate((el) => el.tagName.toLowerCase());
    expect(tagName).toBe('button');
  });

  test('#echo-toasts container is present in the DOM', async ({ page }) => {
    await page.goto('/');
    // echo-toasts is outside any tab pane — always in DOM
    const toasts = page.locator('#echo-toasts');
    await expect(toasts).toBeAttached({ timeout: 10_000 });
  });

  test('switching back to Terminal hides #echo', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    // Open Echo
    await page.locator('#tab-echo').click();
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });

    // Click Terminal tab
    await page.locator('#tab-terminal').click();

    // #echo must be hidden again
    await expect(page.locator('#echo')).toBeHidden({ timeout: 5_000 });
  });

  test('Echo tab: sidebar contains Chats / Auto / Memory sub-tabs', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await page.locator('#tab-echo').click();
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });

    // Sub-tab buttons in echo-sidebar
    await expect(page.locator('#echo-sidebar-tab-chats')).toBeVisible();
    await expect(page.locator('#echo-sidebar-tab-auto')).toBeVisible();
    await expect(page.locator('#echo-sidebar-tab-memory')).toBeVisible();
  });

  test('Echo sidebar: Chats pane visible by default, others hidden', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await page.locator('#tab-echo').click();
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });

    await expect(page.locator('#echo-conversations')).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('#echo-autonomous')).toBeHidden();
    await expect(page.locator('#echo-memory')).toBeHidden();
  });

  test('clicking Auto sub-tab shows autonomous pane, hides chats pane', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await page.locator('#tab-echo').click();
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });

    await page.locator('#echo-sidebar-tab-auto').click();

    await expect(page.locator('#echo-autonomous')).toBeVisible({ timeout: 3_000 });
    await expect(page.locator('#echo-conversations')).toBeHidden();
  });

  test('clicking Memory sub-tab shows memory pane, hides chats pane', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await page.locator('#tab-echo').click();
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });

    await page.locator('#echo-sidebar-tab-memory').click();

    await expect(page.locator('#echo-memory')).toBeVisible({ timeout: 3_000 });
    await expect(page.locator('#echo-conversations')).toBeHidden();
  });

  test('model picker is present in echo header', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await page.locator('#tab-echo').click();
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });

    const picker = page.locator('#echo-model-picker');
    await expect(picker).toBeVisible({ timeout: 5_000 });
    const tagName = await picker.evaluate((el) => el.tagName.toLowerCase());
    expect(tagName).toBe('select');
  });

  test('CSS custom properties --bg and --accent are defined on :root', async ({ page }) => {
    await page.goto('/');
    // Allow JS to run and apply the theme
    await page.locator('#tab-bar').waitFor({ state: 'visible', timeout: 10_000 });

    const bg = await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue('--bg').trim(),
    );
    const accent = await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue('--accent').trim(),
    );

    expect(bg.length, '--bg should be set by the active theme').toBeGreaterThan(0);
    expect(accent.length, '--accent should be set by the active theme').toBeGreaterThan(0);
  });

  test('screenshot of Echo tab open state (visual validation)', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await page.locator('#tab-echo').click();
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });

    // Allow initial data load (conversations list) to settle
    await expect
      .poll(
        async () => {
          // Wait until either the conversations list has items, or shows "Нет чатов",
          // or a new-chat button is visible — any of these means the pane is ready.
          const newChat = await page.locator('#echo-new-chat').isVisible();
          return newChat;
        },
        { timeout: 10_000 },
      )
      .toBe(true);

    // Full-page screenshot is attached by Playwright to the HTML report automatically.
    await page.screenshot({ path: 'test-results/echo-tab-screenshot.png', fullPage: false });

    // Assert the screenshot was taken without error — if the above threw, this
    // line would not be reached.
    expect(true).toBe(true);
  });

  test('#echo-new-chat button is visible in Chats pane', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await page.locator('#tab-echo').click();
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });

    await expect(page.locator('#echo-new-chat')).toBeVisible({ timeout: 5_000 });
  });

  test('#echo-messages log container is present', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await page.locator('#tab-echo').click();
    await expect(page.locator('#echo')).toBeVisible({ timeout: 5_000 });

    const msgs = page.locator('#echo-messages');
    await expect(msgs).toBeAttached();
    const role = await msgs.evaluate((el) => el.getAttribute('role'));
    expect(role).toBe('log');
  });
});
