/**
 * UI-тесты вкладки VK (плагин forge-vk, opt-in `--vk`).
 *
 * ПОЧЕМУ ЗДЕСЬ МОК, А НЕ ЖИВОЙ ПЛАГИН. Сервер для e2e поднимается без флага
 * `--vk` (см. playwright.config.ts), поэтому `GET /api/vk/status` отвечает
 * 404 — и это ПЕРВЫЙ проверяемый сценарий: кнопка Ⓥ обязана остаться скрытой.
 * Всё остальное требует включённого плагина, поэтому статус подменяется через
 * `page.route`. Настоящий вход в VK не выполняется НИКОГДА: токена в CI нет и
 * быть не должно, а `access_token` пользователя — это доступ ко всей его
 * переписке.
 *
 * ВАЖНАЯ ТОНКОСТЬ: перехват ставится ДО `page.goto`. Гейт кнопки
 * (`js/vk/main.js::initVkGate`) спрашивает статус в момент загрузки страницы —
 * `page.route`, поставленный после `goto`, гарантированно опоздает, и тест
 * ловил бы 404 вместо мока.
 *
 * Проверяется:
 *   1. Гейт кнопки: без плагина #vk-toggle скрыт; с моком — виден.
 *   2. Открытие панели: клик показывает #vk, остальные панели скрыты.
 *   3. Экран входа: #vk-login с полем access_token и подсказкой, откуда его
 *      взять (без подсказки пользователь ищет «бот-токен» и не находит).
 *   4. CAPTCHA: при `captcha_required` виден #vk-captcha, а у #vk-captcha-img
 *      адрес начинается с `/api/vk/captcha/` — то есть картинка идёт ЧЕРЕЗ
 *      НАШ ПРОКСИ и браузер пользователя на серверы VK не ходит.
 *   5. Тема: режим ставится через `page.evaluate` (тумблер ходит по кругу и
 *      зависит от темы приложения, а тесту нужен детерминизм); снимаются
 *      скриншоты dark и light.
 *   6. Две панели-мессенджера: Telegram → VK → обратно; `#telegram` и `#vk`
 *      взаимоисключающи, консоль без необработанных исключений.
 *   7. Владение клавиатурой: при открытой панели VK нажатие `j` НЕ уводит
 *      выделение в сайдбар сессий (иначе одно нажатие и листало бы ленту, и
 *      прыгало по сессиям tmux).
 *
 * `.skip()` в файле нет ни одного — по договорённости проекта сценарий либо
 * мокается, либо не пишется.
 */
import { test, expect, Page } from '@playwright/test';

/** Панели, которые обязаны быть скрыты, когда открыта одна из них. */
const HIDDEN_PANE_IDS = ['tasks', 'echo', 'git', 'docker', 'telescope', 'telegram', 'vk', 'bitrix'];

/** Подсказка «где взять токен» приходит С СЕРВЕРА (поле `hint` статуса). */
const TOKEN_HINT =
  'Откройте страницу авторизации VK и скопируйте адресную строку целиком ' +
  '(она начинается с https://oauth.vk.ru/blank.html#access_token=…). ' +
  'Токену нужны права messages и offline.';

/** Ответ включённого плагина без привязанного токена. */
const STATUS_NOT_CONFIGURED = {
  enabled: true,
  login: { state: 'not_configured', error: null, user: null, captcha_sid: null, captcha_img: null },
  api_version: '5.199',
  rate_limit_rps: 3,
  hint: TOKEN_HINT,
};

/** Ответ включённого плагина с привязанным аккаунтом. */
const STATUS_AUTHORIZED = {
  enabled: true,
  login: {
    state: 'authorized',
    error: null,
    user: { id: '1234', first_name: 'Тест', last_name: 'Тестов', screen_name: 'id1234' },
    captcha_sid: null,
    captcha_img: null,
  },
  api_version: '5.199',
  rate_limit_rps: 3,
  hint: TOKEN_HINT,
};

/** Ответ включённого плагина, у которого VK попросил символы с картинки. */
const STATUS_CAPTCHA = {
  enabled: true,
  login: {
    state: 'captcha_required',
    error: 'VK просит подтвердить, что вы человек',
    user: null,
    captcha_sid: '239123',
    // Прямой адрес VK приходит с сервера, но фронт обязан ЕГО ИГНОРИРОВАТЬ и
    // строить путь прокси из sid. Ровно это и проверяет сценарий 4.
    captcha_img: 'https://api.vk.com/captcha.php?sid=239123&s=1',
  },
  api_version: '5.199',
  rate_limit_rps: 3,
  hint: TOKEN_HINT,
};

/** 1x1 PNG — отдаётся вместо картинки captcha, чтобы <img> не ругался. */
const PNG_1X1 = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
  'base64',
);

/**
 * Подменяет поверхность плагина VK.
 *
 * Ставится ДО `goto`: `initVkGate()` спрашивает статус в момент загрузки, и
 * перехват, поставленный позже, опоздает.
 *
 * @param status тело `GET /api/vk/status`
 */
async function mockVkEnabled(page: Page, status: unknown = STATUS_NOT_CONFIGURED): Promise<void> {
  await page.route('**/api/vk/status', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(status),
    });
  });
  // Четыре серверных фильтра — статичный ответ, от сети не зависит.
  await page.route('**/api/vk/filters', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        filters: [
          { key: 'all', title: '~/all' },
          { key: 'unread', title: '~/unread' },
          { key: 'important', title: '~/important' },
          { key: 'unanswered', title: '~/unanswered' },
        ],
      }),
    });
  });
  await page.route('**/api/vk/dialogs*', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ dialogs: [], has_more: false }),
    });
  });
  // Прокси картинки captcha: отдаём настоящие байты PNG, чтобы у <img> не
  // было ошибки загрузки (её видно в консоли и она мешала бы сценарию 6).
  await page.route('**/api/vk/captcha/*', async (route) => {
    await route.fulfill({ status: 200, contentType: 'image/png', body: PNG_1X1 });
  });
}

/** Подменяет статус Telegram — нужен только сценарию сосуществования панелей. */
async function mockTelegramEnabled(page: Page): Promise<void> {
  await page.route('**/api/telegram/status', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        enabled: true,
        state: 'disconnected',
        phone: null,
        hint: null,
        error: null,
        user: null,
      }),
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

/** Открывает вкладку VK и дожидается панели. */
async function openVkTab(page: Page): Promise<void> {
  const toggle = page.locator('#vk-toggle');
  await expect(toggle).toBeVisible({ timeout: 10_000 });
  await toggle.click();
  await expect(page.locator('#vk')).toBeVisible({ timeout: 5_000 });
}

/**
 * Ставит режим темы панели напрямую.
 *
 * Тумблер ◐ ходит по кругу `auto → dark → light → auto`, а результат `auto`
 * зависит от темы самого devforge — тесту нужен детерминированный исход.
 */
async function setVkThemeMode(page: Page, mode: 'auto' | 'dark' | 'light'): Promise<void> {
  await page.evaluate(async (m) => {
    const themeModule = await import('/js/vk/theme.js');
    themeModule.setVkThemeMode(m);
  }, mode);
}

test.describe('VK tab UI', () => {
  test.describe.configure({ mode: 'serial' });

  test('без плагина кнопка Ⓥ остаётся скрытой', async ({ page }) => {
    // Никакого мока: сервер e2e запущен без --vk, статус даёт 404. Именно на
    // этом 404 держится гейт — он и есть контракт «плагин выключен».
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    const status = await page.request.get('/api/vk/status');
    expect(status.status(), 'без флага --vk роут не смонтирован вовсе').toBe(404);

    const toggle = page.locator('#vk-toggle');
    // Кнопка присутствует в разметке всегда — прячет её именно гейт.
    await expect(toggle).toBeAttached();
    await expect(toggle).toBeHidden();
    await expect(page.locator('#vk')).toBeHidden();
  });

  test('404 статуса не считается ошибкой: консоль без исключений', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (e) => pageErrors.push(e.message));

    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });
    await page.waitForTimeout(1_000);

    expect(pageErrors, 'выключенный плагин не должен ронять bootstrap').toEqual([]);
  });

  test('с включённым плагином кнопка Ⓥ появляется', async ({ page }) => {
    await mockVkEnabled(page);
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await expect(page.locator('#vk-toggle')).toBeVisible({ timeout: 10_000 });
    // Бейдж непрочитанного скрыт, пока считать нечего.
    await expect(page.locator('#vk-unread-badge')).toBeHidden();
  });

  test('клик по Ⓥ открывает #vk и скрывает остальные панели', async ({ page }) => {
    await mockVkEnabled(page);
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await expect(page.locator('#vk')).toBeHidden();
    await openVkTab(page);

    for (const id of HIDDEN_PANE_IDS.filter((x) => x !== 'vk')) {
      await expect(page.locator(`#${id}`)).toBeHidden({ timeout: 2_000 });
    }
    await expect(page.locator('#vk-toggle')).toHaveClass(/active/);
  });

  test('каркас панели на месте: titlebar, фильтры, лента, prompt, статусбар', async ({ page }) => {
    await mockVkEnabled(page);
    await page.goto('/');
    await openVkTab(page);

    await expect(page.locator('#vk-titlebar')).toBeVisible();
    await expect(page.locator('#vk-contacts')).toBeVisible();
    await expect(page.locator('#vk-log')).toBeVisible();
    await expect(page.locator('#vk-prompt')).toBeVisible();
    await expect(page.locator('#vk-statusbar')).toBeVisible();
    await expect(page.locator('.vk-prompt-sign')).toHaveText('vk:~$');
    await expect(page.locator('#vk-send')).toContainText('TRANSMIT');

    // Четыре серверных фильтра вместо телеграмных папок — состав константен.
    await expect(page.locator('#vk-dir-list .vk-dir')).toHaveCount(4);
    // В шапке нет обещания сквозного шифрования: у VK его нет, и надпись из
    // телеграмной панели была бы враньём в интерфейсе с перепиской.
    await expect(page.locator('#vk-titlebar')).not.toContainText('ENCRYPTED');
  });

  test('экран входа: поле access_token и подсказка, откуда его взять', async ({ page }) => {
    await mockVkEnabled(page);
    await page.goto('/');
    await openVkTab(page);

    const login = page.locator('#vk-login');
    await expect(login).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('#vk-login-step-token')).toBeVisible();

    const token = page.locator('#vk-token');
    await expect(token).toBeVisible();
    // Токен — секрет: поле обязано быть password, иначе он остаётся на экране
    // и в скриншотах поддержки.
    await expect(token).toHaveAttribute('type', 'password');
    await expect(page.locator('#vk-login-token-submit')).toBeVisible();

    // Подсказка приходит С СЕРВЕРА (поле hint), а не зашита в разметку:
    // правила выдачи scope `messages` меняются, а index.html живёт в кэше PWA.
    await expect(page.locator('#vk-login-hint')).toContainText('access_token');
    await expect(page.locator('#vk-login-hint')).toContainText('messages');

    // Кнопка отвязки недоступна, пока не вошли.
    await expect(page.locator('#vk-logout')).toBeHidden();
    // Captcha — отдельный оверлей, и без кода 14 его быть не должно.
    await expect(page.locator('#vk-captcha')).toBeHidden();
  });

  test('captcha: картинка идёт через прокси, а не с серверов VK', async ({ page }) => {
    // Запоминаем ВСЕ запросы страницы: ни один не имеет права уйти на vk.com.
    const vkHosts: string[] = [];
    page.on('request', (r) => {
      if (/(^|\/\/)([a-z0-9-]+\.)?vk\.(com|ru)\//.test(r.url())) vkHosts.push(r.url());
    });

    await mockVkEnabled(page, STATUS_CAPTCHA);
    await page.goto('/');
    await openVkTab(page);

    const captcha = page.locator('#vk-captcha');
    await expect(captcha).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('#vk-captcha-key')).toBeVisible();

    const src = await page.locator('#vk-captcha-img').getAttribute('src');
    expect(src, 'адрес картинки обязан быть').not.toBeNull();
    expect(
      src as string,
      'картинка обязана идти через прокси бэкенда, а не по прямому captcha_img',
    ).toMatch(/^\/api\/vk\/captcha\//);
    expect(src as string).toContain('239123');
    expect(src as string).not.toContain('vk.com');

    await page.waitForTimeout(500);
    expect(vkHosts, `браузер не должен ходить на серверы VK: ${vkHosts.join(', ')}`).toEqual([]);
  });

  test('тема: режим меняет data-vk-theme, скриншоты обеих палитр', async ({ page }) => {
    await mockVkEnabled(page);
    await page.goto('/');
    await openVkTab(page);

    await setVkThemeMode(page, 'dark');
    await expect(page.locator('#vk')).toHaveAttribute('data-vk-theme', 'dark');
    await expect(page.locator('#vk-theme-toggle')).toHaveAttribute('data-vk-theme-mode', 'dark');
    await page.locator('#vk').screenshot({ path: 'test-results/vk-dark.png' });

    await setVkThemeMode(page, 'light');
    await expect(page.locator('#vk')).toHaveAttribute('data-vk-theme', 'light');
    await expect(page.locator('#vk-theme-toggle')).toHaveAttribute('data-vk-theme-mode', 'light');
    await page.locator('#vk').screenshot({ path: 'test-results/vk-light.png' });

    // Режим переживает перезагрузку страницы.
    await page.reload();
    await expect(page.locator('#vk-toggle')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('#vk')).toHaveAttribute('data-vk-theme', 'light');

    // Возвращаем auto: localStorage общий на контекст, а describe идёт serial.
    await setVkThemeMode(page, 'auto');
  });

  test('тумблер ◐ кликается поверх оверлея входа и идёт по кругу', async ({ page }) => {
    await mockVkEnabled(page);
    await page.goto('/');
    await openVkTab(page);
    await expect(page.locator('#vk-login')).toBeVisible();
    await setVkThemeMode(page, 'auto');

    // Оверлей логина не имеет права перехватывать клики по titlebar: иначе на
    // светлой теме devforge панель залипает в чужой палитре до самого входа.
    const toggle = page.locator('#vk-theme-toggle');
    await expect(toggle).toHaveAttribute('data-vk-theme-mode', 'auto');
    await toggle.click({ timeout: 5_000 });
    await expect(toggle).toHaveAttribute('data-vk-theme-mode', 'dark');
    await toggle.click();
    await expect(toggle).toHaveAttribute('data-vk-theme-mode', 'light');
    await toggle.click();
    await expect(toggle).toHaveAttribute('data-vk-theme-mode', 'auto');
  });

  test('две панели-мессенджера взаимоисключающи: Telegram → VK → обратно', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (e) => pageErrors.push(e.message));

    await mockTelegramEnabled(page);
    await mockVkEnabled(page);
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('#telegram-toggle')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('#vk-toggle')).toBeVisible({ timeout: 10_000 });

    // Telegram.
    await page.locator('#telegram-toggle').click();
    await expect(page.locator('#telegram')).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('#vk')).toBeHidden();

    // VK: телеграмная панель обязана погаснуть.
    await page.locator('#vk-toggle').click();
    await expect(page.locator('#vk')).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('#telegram')).toBeHidden();

    // И обратно — каркас обеих панелей пережил переключение.
    await page.locator('#telegram-toggle').click();
    await expect(page.locator('#telegram')).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('#vk')).toBeHidden();
    await expect(page.locator('#tg-prompt')).toBeVisible();

    await page.locator('#vk-toggle').click();
    await expect(page.locator('#vk')).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('#vk-prompt')).toBeVisible();
    await expect(page.locator('#vk-login')).toBeVisible();

    // Взаимоисключающи по атрибуту hidden, а не только визуально.
    const hidden = await page.evaluate(() => ({
      telegram: (document.getElementById('telegram') as HTMLElement).hidden,
      vk: (document.getElementById('vk') as HTMLElement).hidden,
    }));
    expect(hidden.telegram, 'одновременно открытыми панели быть не могут').toBe(true);
    expect(hidden.vk).toBe(false);

    expect(pageErrors, 'необработанных исключений быть не должно').toEqual([]);
  });

  test('владение клавиатурой: при открытом #vk «j» не двигает сайдбар сессий', async ({ page }) => {
    // Аккаунт привязан: оверлей входа закрыт, иначе панель клавиатурой не
    // владеет по построению (в форме логина j/k принадлежат полю).
    await mockVkEnabled(page, STATUS_AUTHORIZED);
    await page.goto('/');
    await openVkTab(page);
    await expect(page.locator('#vk-login')).toBeHidden({ timeout: 5_000 });

    // Фокус нигде (на body) — панель считает клавиши своими.
    await page.evaluate(() => {
      const el = document.activeElement as HTMLElement | null;
      if (el && typeof el.blur === 'function') el.blur();
    });
    const owns = await page.evaluate(() => {
      const app = (window as unknown as { ForgeApp?: { vkKeys?: { owns?: () => boolean } } }).ForgeApp;
      return !!(app && app.vkKeys && typeof app.vkKeys.owns === 'function' && app.vkKeys.owns());
    });
    expect(owns, 'открытая панель VK обязана владеть клавиатурой').toBe(true);

    await page.keyboard.press('j');
    await page.keyboard.press('k');
    await page.waitForTimeout(200);

    const active = await page.evaluate(() => {
      const el = document.activeElement as HTMLElement | null;
      return {
        isSessionItem: !!(el && el.classList && el.classList.contains('session-item')),
        insideVk: !!(el && document.getElementById('vk')?.contains(el)),
        tag: el ? el.tagName : null,
      };
    });
    expect(
      active.isSessionItem,
      'vim-навигация hotkeys.js обязана молчать, пока открыта панель мессенджера',
    ).toBe(false);
    expect(active.insideVk || active.tag === 'BODY').toBe(true);

    // А уход фокусом в сайдбар возвращает клавиши vim'у — иначе открытая
    // вкладка VK отняла бы j/k у всего остального интерфейса.
    await page.evaluate(() => {
      const item = document.querySelector('#sidebar') as HTMLElement | null;
      if (item && typeof item.focus === 'function') {
        item.setAttribute('tabindex', '-1');
        item.focus();
      }
    });
    const ownsAfter = await page.evaluate(() => {
      const app = (window as unknown as { ForgeApp?: { vkKeys?: { owns?: () => boolean } } }).ForgeApp;
      return !!(app && app.vkKeys && typeof app.vkKeys.owns === 'function' && app.vkKeys.owns());
    });
    expect(ownsAfter, 'фокус вне панели — клавиши больше не её').toBe(false);
  });

  test('панель занимает высоту #main и не даёт горизонтального скролла', async ({ page }) => {
    await mockVkEnabled(page);
    await page.goto('/');
    await openVkTab(page);

    const metrics = await page.evaluate(() => {
      const el = document.getElementById('vk') as HTMLElement;
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

  test('вкладка не ходит во внешние домены', async ({ page }) => {
    const external: string[] = [];
    page.on('request', (r) => {
      const url = r.url();
      if (!/^https?:\/\/(127\.0\.0\.1|localhost)/.test(url) && !url.startsWith('data:')) {
        external.push(url);
      }
    });

    await mockVkEnabled(page);
    await page.goto('/');
    await openVkTab(page);
    await page.waitForTimeout(1_000);

    expect(external, `внешние запросы запрещены: ${external.join(', ')}`).toEqual([]);
  });
});
