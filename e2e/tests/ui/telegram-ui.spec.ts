/**
 * UI-тесты вкладки Telegram (плагин forge-telegram, opt-in `--telegram`).
 *
 * ПОЧЕМУ ЗДЕСЬ МОК, А НЕ ЖИВОЙ ПЛАГИН. Сервер для e2e поднимается без флага
 * `--telegram` (см. playwright.config.ts), поэтому `GET /api/telegram/status`
 * отвечает 404 — и это ПЕРВЫЙ проверяемый сценарий: кнопка ✈ обязана
 * остаться скрытой. Всё остальное требует включённого плагина, поэтому
 * статус подменяется через `page.route`: фронт получает `{enabled:true}` и
 * ведёт себя ровно так же, как с настоящим сервером. Настоящий вход в
 * Telegram не выполняется НИКОГДА — учётных данных в CI нет и быть не должно.
 *
 * Проверяется:
 *   1. Гейт кнопки: без плагина #telegram-toggle скрыт; с моком — виден.
 *   2. Открытие панели: клик показывает #telegram, остальные панели скрыты.
 *   3. Login-оверлей: при `state != authorized` виден #tg-login на шаге 1 с
 *      полями api_id / api_hash / phone и пояснением про my.telegram.org.
 *   4. Тема: тумблер #tg-theme-toggle меняет data-tg-theme на #telegram;
 *      снимаются скриншоты dark и light для дизайн-сверки.
 *   5. Возврат на другую вкладку и обратно не ломает панель; консоль чиста.
 *
 * `.skip()` в файле нет ни одного — по договорённости проекта сценарий либо
 * мокается, либо не пишется.
 */
import { test, expect, Page } from '@playwright/test';

// `vk` добавлен вместе с плагином ВКонтакте (Ф12): панели мессенджеров
// взаимоисключающи, и открытый Telegram обязан прятать VK так же, как всё
// остальное. Без этой строки спека Telegram перестала бы отражать реальность.
const HIDDEN_PANE_IDS = ['tasks', 'echo', 'git', 'docker', 'telescope', 'telegram', 'vk'];

/** Ответ, который отдаёт включённый плагин без привязанного аккаунта. */
const STATUS_DISCONNECTED = {
  enabled: true,
  state: 'disconnected',
  phone: null,
  hint: null,
  error: null,
  user: null,
};

/**
 * Подменяет ответы плагина: статус — «включён, но не авторизован», список
 * диалогов — контрактная 409.
 *
 * Ставится ДО `goto`, потому что `initTelegramGate()` спрашивает статус в
 * момент загрузки страницы: перехват, поставленный позже, опоздает.
 */
async function mockTelegramEnabled(page: Page): Promise<void> {
  await page.route('**/api/telegram/status', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(STATUS_DISCONNECTED),
    });
  });
  await page.route('**/api/telegram/dialogs*', async (route) => {
    await route.fulfill({
      status: 409,
      contentType: 'application/json',
      body: JSON.stringify({
        error: { code: 'not_authorized', message: 'Telegram account is not linked yet' },
      }),
    });
  });
}

/** Открывает вкладку и дожидается панели. */
async function openTelegramTab(page: Page): Promise<void> {
  const toggle = page.locator('#telegram-toggle');
  await expect(toggle).toBeVisible({ timeout: 10_000 });
  await toggle.click();
  await expect(page.locator('#telegram')).toBeVisible({ timeout: 5_000 });
}

/** Ставит режим темы панели напрямую — тумблер ходит по кругу и зависит от
 *  темы приложения, а тесту нужен детерминированный результат. */
async function setTgThemeMode(page: Page, mode: 'auto' | 'dark' | 'light'): Promise<void> {
  await page.evaluate(async (m) => {
    const themeModule = await import('/js/telegram/theme.js');
    themeModule.setTgThemeMode(m);
  }, mode);
}

test.describe('Telegram tab UI', () => {
  test.describe.configure({ mode: 'serial' });

  test('без плагина кнопка ✈ остаётся скрытой', async ({ page }) => {
    // Никакого мока: сервер e2e запущен без --telegram, статус даёт 404.
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    const toggle = page.locator('#telegram-toggle');
    // Кнопка присутствует в разметке всегда — прячет её именно гейт.
    await expect(toggle).toBeAttached();
    await expect(toggle).toBeHidden();
    // И сама панель тоже скрыта.
    await expect(page.locator('#telegram')).toBeHidden();
  });

  test('404 статуса не считается ошибкой: консоль без исключений', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (e) => pageErrors.push(e.message));

    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });
    await page.waitForTimeout(1_000);

    expect(pageErrors, 'выключенный плагин не должен ронять bootstrap').toEqual([]);
  });

  test('с включённым плагином кнопка ✈ появляется', async ({ page }) => {
    await mockTelegramEnabled(page);
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await expect(page.locator('#telegram-toggle')).toBeVisible({ timeout: 10_000 });
  });

  test('клик по ✈ открывает #telegram и скрывает остальные панели', async ({ page }) => {
    await mockTelegramEnabled(page);
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await expect(page.locator('#telegram')).toBeHidden();
    await openTelegramTab(page);

    for (const id of HIDDEN_PANE_IDS.filter((x) => x !== 'telegram')) {
      await expect(page.locator(`#${id}`)).toBeHidden({ timeout: 2_000 });
    }
    // Кнопка вкладки подсвечена как активная.
    await expect(page.locator('#telegram-toggle')).toHaveClass(/active/);
  });

  test('каркас панели на месте: titlebar, контакты, лента, prompt, статусбар', async ({ page }) => {
    await mockTelegramEnabled(page);
    await page.goto('/');
    await openTelegramTab(page);

    await expect(page.locator('#tg-titlebar')).toBeVisible();
    await expect(page.locator('#tg-contacts')).toBeVisible();
    await expect(page.locator('#tg-log')).toBeVisible();
    await expect(page.locator('#tg-prompt')).toBeVisible();
    await expect(page.locator('#tg-statusbar')).toBeVisible();
    await expect(page.locator('.tg-prompt-sign')).toHaveText('tg:~$');
    await expect(page.locator('#tg-send')).toContainText('TRANSMIT');
  });

  test('login-оверлей: шаг 1 с полями api_id / api_hash / phone', async ({ page }) => {
    await mockTelegramEnabled(page);
    await page.goto('/');
    await openTelegramTab(page);

    const login = page.locator('#tg-login');
    await expect(login).toBeVisible({ timeout: 5_000 });

    // Шаг 1 открыт, шаги 2 и 3 — нет.
    await expect(page.locator('#tg-login-step-creds')).toBeVisible();
    await expect(page.locator('#tg-login-step-code')).toBeHidden();
    await expect(page.locator('#tg-login-step-password')).toBeHidden();

    await expect(page.locator('#tg-api-id')).toBeVisible();
    await expect(page.locator('#tg-api-hash')).toBeVisible();
    await expect(page.locator('#tg-phone')).toBeVisible();
    // Пояснение обязано вести на my.telegram.org: без него пользователь не
    // поймёт, где взять api_id, и решит, что нужен бот-токен.
    await expect(login).toContainText('my.telegram.org');
    // Пароль и хэш не должны быть видимым текстом.
    await expect(page.locator('#tg-api-hash')).toHaveAttribute('type', 'password');
    // Кнопка выхода недоступна, пока не вошли.
    await expect(page.locator('#tg-logout')).toBeHidden();
  });

  test('оверлей логина не перекрывает titlebar и статусбар', async ({ page }) => {
    await mockTelegramEnabled(page);
    await page.goto('/');
    await openTelegramTab(page);
    await expect(page.locator('#tg-login')).toBeVisible();

    // Тумблер темы обязан оставаться кликабельным ДО входа в аккаунт: иначе
    // на светлой теме devforge панель залипает в чужой палитре навсегда.
    // Два клика от 'auto' дают именно 'light' — то есть клики дошли до
    // кнопки, а не были перехвачены оверлеем.
    await setTgThemeMode(page, 'auto');
    const toggle = page.locator('#tg-theme-toggle');
    await toggle.click({ timeout: 5_000 });
    await toggle.click();
    await expect(toggle).toHaveAttribute('data-tg-theme-mode', 'light');
    await expect(page.locator('#telegram')).toHaveAttribute('data-tg-theme', 'light');
    await setTgThemeMode(page, 'auto');

    // Статусбар виден и показывает состояние соединения.
    await expect(page.locator('#tg-mode')).toBeVisible();
    await expect(page.locator('#tg-mode')).toHaveText(/CONNECTED|RECONNECTING|OFFLINE/);
  });

  test('тумблер темы переключает data-tg-theme и режим сохраняется', async ({ page }) => {
    await mockTelegramEnabled(page);
    await page.goto('/');
    await openTelegramTab(page);

    await setTgThemeMode(page, 'dark');
    await expect(page.locator('#telegram')).toHaveAttribute('data-tg-theme', 'dark');
    await expect(page.locator('#tg-theme-toggle')).toHaveAttribute('data-tg-theme-mode', 'dark');

    await setTgThemeMode(page, 'light');
    await expect(page.locator('#telegram')).toHaveAttribute('data-tg-theme', 'light');
    await expect(page.locator('#tg-theme-toggle')).toHaveAttribute('data-tg-theme-mode', 'light');

    // Режим переживает перезагрузку страницы.
    await page.reload();
    await expect(page.locator('#telegram-toggle')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('#telegram')).toHaveAttribute('data-tg-theme', 'light');

    // Возвращаем auto, чтобы не влиять на соседние тесты (localStorage общий
    // на контекст, а describe идёт serial).
    await setTgThemeMode(page, 'auto');
  });

  test('клик по ◐ идёт по кругу auto → dark → light → auto', async ({ page }) => {
    await mockTelegramEnabled(page);
    await page.goto('/');
    await openTelegramTab(page);
    await setTgThemeMode(page, 'auto');

    const toggle = page.locator('#tg-theme-toggle');
    await expect(toggle).toHaveAttribute('data-tg-theme-mode', 'auto');
    await toggle.click();
    await expect(toggle).toHaveAttribute('data-tg-theme-mode', 'dark');
    await toggle.click();
    await expect(toggle).toHaveAttribute('data-tg-theme-mode', 'light');
    await toggle.click();
    await expect(toggle).toHaveAttribute('data-tg-theme-mode', 'auto');
  });

  test('скриншоты обеих тем для дизайн-сверки', async ({ page }) => {
    await mockTelegramEnabled(page);
    await page.goto('/');
    await openTelegramTab(page);

    await setTgThemeMode(page, 'dark');
    await expect(page.locator('#telegram')).toHaveAttribute('data-tg-theme', 'dark');
    await page.locator('#telegram').screenshot({ path: 'test-results/telegram-dark.png' });

    await setTgThemeMode(page, 'light');
    await expect(page.locator('#telegram')).toHaveAttribute('data-tg-theme', 'light');
    await page.locator('#telegram').screenshot({ path: 'test-results/telegram-light.png' });

    await setTgThemeMode(page, 'auto');
  });

  test('уход на другую вкладку и возврат не ломают панель', async ({ page }) => {
    const consoleErrors: string[] = [];
    const pageErrors: string[] = [];
    page.on('console', (m) => {
      if (m.type() === 'error') consoleErrors.push(`${m.text()} @ ${m.location().url}`);
    });
    page.on('pageerror', (e) => pageErrors.push(e.message));

    await mockTelegramEnabled(page);
    await page.goto('/');
    await openTelegramTab(page);

    await page.locator('#tab-terminal').click();
    await expect(page.locator('#telegram')).toBeHidden({ timeout: 5_000 });

    await page.locator('#telegram-toggle').click();
    await expect(page.locator('#telegram')).toBeVisible({ timeout: 5_000 });
    // Каркас пережил переключение.
    await expect(page.locator('#tg-prompt')).toBeVisible();
    await expect(page.locator('#tg-login')).toBeVisible();

    expect(pageErrors, 'необработанных исключений быть не должно').toEqual([]);
    // Фоновый шум изолированного инстанса (PWA-конфиг, echo-статистика,
    // todos) — не про вкладку Telegram и в приёмку не входит. Сюда же
    // `api/vk/status`: сервер e2e поднят и без `--vk`, а 404 на этом роуте —
    // КОНТРАКТ выключенного плагина VK (по нему прячется кнопка Ⓥ). Строку в
    // консоль пишет браузер на неудачный запрос ресурса, а не приложение:
    // сам гейт этот случай проглатывает молча.
    const relevant = consoleErrors.filter(
      (e) => !/pwa\/config|echo\/stats|api\/todos|favicon|api\/vk\/status/.test(e),
    );
    expect(relevant, `лишние ошибки в консоли: ${relevant.join('; ')}`).toEqual([]);
  });

  test('панель занимает высоту #main и не даёт горизонтального скролла', async ({ page }) => {
    await mockTelegramEnabled(page);
    await page.goto('/');
    await openTelegramTab(page);

    const metrics = await page.evaluate(() => {
      const el = document.getElementById('telegram') as HTMLElement;
      const r = el.getBoundingClientRect();
      return {
        height: Math.round(r.height),
        width: Math.round(r.width),
        overflowX: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      };
    });
    expect(metrics.height, 'панель не схлопнулась').toBeGreaterThan(200);
    expect(metrics.width).toBeGreaterThan(200);
    expect(metrics.overflowX, 'горизонтального скролла быть не должно').toBeLessThanOrEqual(0);
  });

  test('завендоренный шрифт отдаётся сервером с font/woff2', async ({ request }) => {
    const resp = await request.get('/vendor/fonts/jetbrains-mono/JetBrainsMono-Regular.woff2');
    expect(resp.status()).toBe(200);
    expect(resp.headers()['content-type']).toBe('font/woff2');
    const body = await resp.body();
    expect(body.subarray(0, 4).toString('latin1')).toBe('wOF2');
  });

  test('вкладка не ходит во внешние домены', async ({ page }) => {
    const external: string[] = [];
    page.on('request', (r) => {
      const url = r.url();
      if (!/^https?:\/\/(127\.0\.0\.1|localhost)/.test(url) && !url.startsWith('data:')) {
        external.push(url);
      }
    });

    await mockTelegramEnabled(page);
    await page.goto('/');
    await openTelegramTab(page);
    await page.waitForTimeout(1_000);

    expect(external, `внешние запросы запрещены: ${external.join(', ')}`).toEqual([]);
  });
});
