/**
 * UI-тесты вкладки «Битрикс24» (плагин forge-bitrix, opt-in `--bitrix`).
 *
 * ПОЧЕМУ ЗДЕСЬ МОК, А НЕ ЖИВОЙ ПЛАГИН. Сервер для e2e поднимается БЕЗ флага
 * `--bitrix` (см. playwright.config.ts), поэтому `GET /api/bitrix/status`
 * отвечает 404 — и это ПЕРВЫЙ проверяемый сценарий: кнопка Ⓑ обязана остаться
 * скрытой. Всё остальное требует включённого плагина, поэтому статус
 * подменяется через `page.route`. Настоящее подключение портала не выполняется
 * НИКОГДА: вебхук — это доступ ко всей переписке сотрудника, и в CI его быть не
 * может.
 *
 * ВАЖНАЯ ТОНКОСТЬ: перехват ставится ДО `page.goto`. Гейт кнопки
 * (`js/bitrix/main.js::initB24Gate`) спрашивает статус в момент загрузки
 * страницы — `page.route`, поставленный после `goto`, гарантированно опоздает,
 * и тест ловил бы 404 вместо мока.
 *
 * ПОЧЕМУ ПРОВЕРКИ ИДУТ ЧЕРЕЗ `_debugState()`. Снимок состояния собирается точкой
 * сборки панели из `_debug*` каждого модуля. Тест, который вместо этого ищет
 * третий `li` в четвёртом `div`, ломается от любой косметической правки и
 * перестаёт что-либо значить. DOM проверяется только там, где он и есть
 * предмет проверки (видимость панели, наличие поля ввода вебхука, атрибут
 * темы).
 *
 * Проверяется:
 *   1. Гейт кнопки: без плагина #bitrix-toggle скрыт; с моком — виден.
 *   2. Открытие панели: клик показывает #bitrix, остальные панели скрыты.
 *   3. Каркас: titlebar, четыре ЛОКАЛЬНЫЕ директории, лента, prompt, статусбар.
 *   4. Экран подключения: поле вебхука типа password и подсказка С СЕРВЕРА,
 *      включая предупреждение про «одного сотрудника» (Р15).
 *   5. Список диалогов и директория задач: строки из мока, счётчики, карточка.
 *   6. Тема: режим ставится через `page.evaluate` (тумблер ходит по кругу и
 *      зависит от темы приложения, а тесту нужен детерминизм); скриншоты обеих.
 *   7. Настройки: вкладка «Битрикс24» с разделом видимости и таблицей клавиш.
 *   8. Белый список: сохранённая настройка убирает диалог из колонки, из
 *      счётчиков директорий и из бейджа — все точки разом, через _debugState().
 *   9. Хоткеи: при открытой панели `j` НЕ уводит выделение в сайдбар сессий,
 *      а уход фокуса наружу возвращает клавиши vim'у.
 *  10. Мобильная раскладка: на узком экране колонка off-canvas и открывается ☰.
 *  11. Три панели-мессенджера взаимоисключающи, консоль без исключений.
 *
 * `.skip()` в файле нет ни одного — по договорённости проекта сценарий либо
 * мокается, либо не пишется.
 */
import { test, expect, Page } from '@playwright/test';

/** Панели, которые обязаны быть скрыты, когда открыта одна из них. */
const HIDDEN_PANE_IDS = ['tasks', 'echo', 'git', 'docker', 'telescope', 'telegram', 'vk', 'bitrix'];

/** Подсказка «где взять вебхук» приходит С СЕРВЕРА (поле `hint` статуса). */
const WEBHOOK_HINT =
  'В Битрикс24: Разработчикам → Другое → Входящий вебхук. ' +
  'Отметьте права «Чат и уведомления» (im) и «Задачи» (task), сохраните и вставьте сюда ' +
  'адрес вида https://ваш-портал.bitrix24.ru/rest/1/код/ — подойдёт и адрес вместе с именем ' +
  'метода, и без хвостового слэша. Вебхук выпускается от имени одного сотрудника: все, кто ' +
  'откроет этот devforge, увидят его переписку.';

const LOGOUT_HINT =
  'Выход стирает вебхук только из devforge. На портале он остаётся действующим — ' +
  'отозвать его можно лишь там же, где он был создан.';

const CAPABILITIES = {
  incoming_typing: false,
  realtime_presence: false,
  realtime_edits: false,
  reactions: false,
  reply_to_on_read: false,
  forwarding: false,
  tasks: true,
};

/** Ответ включённого плагина без привязанного вебхука. */
const STATUS_NOT_CONFIGURED = {
  enabled: true,
  login: { state: 'not_configured', error: null, user: null },
  connected: false,
  max_upload_bytes: 26214400,
  rate_limit_rps: 2,
  capabilities: CAPABILITIES,
  hint: WEBHOOK_HINT,
  logout_hint: LOGOUT_HINT,
};

/** Ответ включённого плагина с подключённым порталом. */
const STATUS_AUTHORIZED = {
  enabled: true,
  login: {
    state: 'authorized',
    error: null,
    user: { id: '1', name: 'Тест', last_name: 'Тестов' },
  },
  connected: true,
  max_upload_bytes: 26214400,
  rate_limit_rps: 2,
  capabilities: CAPABILITIES,
  hint: WEBHOOK_HINT,
  logout_hint: LOGOUT_HINT,
};

/**
 * Список диалогов: личный, групповой чат и ЧАТ ЗАДАЧИ.
 *
 * Третья строка — то, чего нет ни у Telegram, ни у VK: чат задачи это обычный
 * IM-чат с `entity_type = "TASKS_TASK"`, и по нему собирается директория
 * `~/tasks`.
 */
const DIALOGS = {
  dialogs: [
    {
      id: '19',
      chat_id: null,
      kind: 'user',
      title: 'Иванов Иван',
      avatar_url: null,
      color: '#DF532D',
      unread_count: 3,
      last_message: null,
      date_update: '2026-08-06T10:00:00+03:00',
      pinned: false,
      muted: false,
      can_write: true,
      members: null,
      entity_type: null,
      entity_id: null,
      task_id: null,
      entity_link_url: null,
      role: null,
      online: false,
    },
    {
      id: 'chat3',
      chat_id: 3,
      kind: 'chat',
      title: 'Общий чат',
      avatar_url: null,
      color: '#2FC6F6',
      unread_count: 1,
      last_message: null,
      date_update: '2026-08-06T09:30:00+03:00',
      pinned: false,
      muted: false,
      can_write: true,
      members: 5,
      entity_type: null,
      entity_id: null,
      task_id: null,
      entity_link_url: null,
      role: 'member',
      online: false,
    },
    {
      id: 'chat7',
      chat_id: 7,
      kind: 'task',
      title: 'Починить сборку',
      avatar_url: null,
      color: '#8BC84B',
      unread_count: 5,
      last_message: null,
      date_update: '2026-08-06T09:00:00+03:00',
      pinned: false,
      muted: false,
      can_write: true,
      members: 2,
      entity_type: 'TASKS_TASK',
      entity_id: 'TASK|1',
      task_id: '1',
      entity_link_url: '/company/personal/user/1/tasks/task/view/1/',
      role: 'member',
      online: false,
    },
  ],
  has_more: false,
};

/** Четыре ЛОКАЛЬНЫЕ директории + разбивка счётчиков. */
const FILTERS = {
  filters: [
    { key: 'all', title: '~/all', unread: 9 },
    { key: 'chats', title: '~/chats', unread: 4 },
    { key: 'tasks', title: '~/tasks', unread: 5 },
    { key: 'unread', title: '~/unread', unread: 9 },
  ],
  counters: { total: 9, chat: 6, dialog: 3, tasks: 5, notify: 0 },
};

/** Снимок состояния панели, собранный её же точкой сборки. */
type DebugState = {
  enabled: boolean;
  loginState: string;
  activeDir: string;
  dialogs: { loaded: number; visible: number; dir: string };
  filter: { enabled: boolean; active: boolean; kinds: string[]; peers: string[] };
  keys: { owns: boolean; enabled: boolean };
  mobile: { narrow: boolean; open: boolean; maxWidth: number };
  theme: { mode: string; applied: string };
  counters: { total: number };
};

async function debugState(page: Page): Promise<DebugState> {
  return page.evaluate(async () => {
    const mod = await import('/js/bitrix/main.js');
    return mod._debugState();
  }) as Promise<DebugState>;
}

/**
 * Подменяет поверхность плагина.
 *
 * Ставится ДО `goto`: `initB24Gate()` спрашивает статус в момент загрузки, и
 * перехват, поставленный позже, опоздает.
 */
async function mockBitrixEnabled(page: Page, status: unknown = STATUS_NOT_CONFIGURED): Promise<void> {
  await page.route('**/api/bitrix/status', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(status),
    });
  });
  await page.route('**/api/bitrix/filters', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(FILTERS),
    });
  });
  await page.route('**/api/bitrix/dialogs?*', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(DIALOGS),
    });
  });
  await page.route('**/api/bitrix/dialogs', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(DIALOGS),
    });
  });
  // Лента открытого диалога — пустая: сценариев отправки здесь нет, а
  // необработанный запрос давал бы ошибку в консоли и ломал сценарий 11.
  await page.route('**/api/bitrix/dialogs/*/messages*', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ messages: [], has_more: false }),
    });
  });
  await page.route('**/api/bitrix/tasks/counters', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ expired: 0, new_comments: 0, mentioned: 0 }),
    });
  });
  await page.route('**/api/bitrix/tasks?*', async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ tasks: [], has_more: false }),
    });
  });
}

/** Открывает вкладку «Битрикс24» и дожидается панели. */
async function openBitrixTab(page: Page): Promise<void> {
  const toggle = page.locator('#bitrix-toggle');
  await expect(toggle).toBeVisible({ timeout: 10_000 });
  await toggle.click();
  await expect(page.locator('#bitrix')).toBeVisible({ timeout: 5_000 });
}

/**
 * Открывает вкладку на УЗКОМ экране.
 *
 * Кнопка Ⓑ живёт в `#settings-bar` сайдбара, и при ширине 420px тот уезжает
 * за край: Playwright честно отказывается кликать «element is outside of the
 * viewport». Клик через `page.evaluate` проверяет ровно то, что нужно этому
 * сценарию (раскладку самой панели), не превращая тест в проверку сайдбара.
 */
async function openBitrixTabViaJs(page: Page): Promise<void> {
  await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 15_000 });
  // Именно `toBeVisible`, а не `toBeAttached`: кнопка присутствует в разметке
  // всегда, и клик по ней ДО ответа гейта открыл бы панель раньше, чем панель
  // узнала, что плагин включён.
  await expect(page.locator('#bitrix-toggle')).toBeVisible({ timeout: 15_000 });
  await page.evaluate(() => {
    const btn = document.getElementById('bitrix-toggle') as HTMLElement | null;
    if (btn) btn.click();
  });
  await expect(page.locator('#bitrix')).toBeVisible({ timeout: 5_000 });
}

/**
 * Ставит режим темы панели напрямую.
 *
 * Тумблер ◐ ходит по кругу `auto → dark → light → auto`, а результат `auto`
 * зависит от темы самого devforge — тесту нужен детерминированный исход.
 */
async function setB24ThemeMode(page: Page, mode: 'auto' | 'dark' | 'light'): Promise<void> {
  await page.evaluate(async (m) => {
    const themeModule = await import('/js/bitrix/theme.js');
    themeModule.setB24ThemeMode(m);
  }, mode);
}

/** Кладёт настройки в живой контракт `window.ForgeApp.state.userSettings`. */
async function setUserSettings(page: Page, patch: Record<string, unknown>): Promise<void> {
  await page.evaluate(async (p) => {
    const app = (window as unknown as {
      ForgeApp?: { state?: { userSettings?: Record<string, unknown> | null } };
    }).ForgeApp;
    if (!app || !app.state) return;
    app.state.userSettings = Object.assign({}, app.state.userSettings || {}, p);
    // Панель кэширует разбор по СОДЕРЖИМОМУ настроек, но публичный контракт
    // применения — тот же, которым пользуется форма настроек.
    const filter = (window as unknown as { ForgeApp?: { b24Filter?: { refresh?: () => void } } })
      .ForgeApp?.b24Filter;
    if (filter && typeof filter.refresh === 'function') filter.refresh();
  }, patch);
}

test.describe('Bitrix24 tab UI', () => {
  test.describe.configure({ mode: 'serial' });

  test('без плагина кнопка Ⓑ остаётся скрытой', async ({ page }) => {
    // Никакого мока: сервер e2e запущен без --bitrix, статус даёт 404. Именно
    // на этом 404 держится гейт — он и есть контракт «плагин выключен».
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    const status = await page.request.get('/api/bitrix/status');
    expect(status.status(), 'без флага --bitrix роут не смонтирован вовсе').toBe(404);

    const toggle = page.locator('#bitrix-toggle');
    // Кнопка присутствует в разметке всегда — прячет её именно гейт.
    await expect(toggle).toBeAttached();
    await expect(toggle).toBeHidden();
    await expect(page.locator('#bitrix')).toBeHidden();
  });

  test('404 статуса не считается ошибкой: консоль без исключений', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (e) => pageErrors.push(e.message));

    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });
    await page.waitForTimeout(1_000);

    expect(pageErrors, 'выключенный плагин не должен ронять bootstrap').toEqual([]);
  });

  test('с включённым плагином кнопка Ⓑ появляется', async ({ page }) => {
    await mockBitrixEnabled(page);
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await expect(page.locator('#bitrix-toggle')).toBeVisible({ timeout: 10_000 });
    // Бейдж непрочитанного скрыт, пока считать нечего.
    await expect(page.locator('#b24-unread-badge')).toBeHidden();
  });

  test('клик по Ⓑ открывает #bitrix и скрывает остальные панели', async ({ page }) => {
    await mockBitrixEnabled(page);
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });

    await expect(page.locator('#bitrix')).toBeHidden();
    await openBitrixTab(page);

    for (const id of HIDDEN_PANE_IDS.filter((x) => x !== 'bitrix')) {
      await expect(page.locator(`#${id}`)).toBeHidden({ timeout: 2_000 });
    }
    await expect(page.locator('#bitrix-toggle')).toHaveClass(/active/);
  });

  test('каркас панели: titlebar, четыре директории, лента, prompt, статусбар', async ({ page }) => {
    await mockBitrixEnabled(page);
    await page.goto('/');
    await openBitrixTab(page);

    await expect(page.locator('#b24-titlebar')).toBeVisible();
    await expect(page.locator('#b24-contacts')).toBeVisible();
    await expect(page.locator('#b24-log')).toBeVisible();
    await expect(page.locator('#b24-prompt')).toBeVisible();
    await expect(page.locator('#b24-statusbar')).toBeVisible();

    // Четыре ЛОКАЛЬНЫЕ директории — состав константен и от портала не зависит.
    await expect(page.locator('#b24-dir-list .b24-dir')).toHaveCount(4);
    // В шапке нет обещания сквозного шифрования: у портала его нет, и надпись
    // из телеграмной панели была бы враньём в интерфейсе с перепиской.
    await expect(page.locator('#b24-titlebar')).not.toContainText('ENCRYPTED');
    await expect(page.locator('#b24-titlebar')).toContainText('REST WEBHOOK');
  });

  test('экран подключения: поле вебхука password и подсказка с сервера', async ({ page }) => {
    await mockBitrixEnabled(page);
    await page.goto('/');
    await openBitrixTab(page);

    const login = page.locator('#b24-login');
    await expect(login).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('#b24-login-step-webhook')).toBeVisible();

    const webhook = page.locator('#b24-webhook');
    await expect(webhook).toBeVisible();
    // Вебхук — секрет ЦЕЛИКОМ: поле обязано быть password, иначе он остаётся
    // на экране и в скриншотах поддержки.
    await expect(webhook).toHaveAttribute('type', 'password');
    await expect(page.locator('#b24-login-webhook-submit')).toBeVisible();

    // Подсказка приходит С СЕРВЕРА (поле hint), а не зашита в разметку:
    // интерфейс портала меняется, а index.html живёт в кэше PWA неделями.
    const hint = page.locator('#b24-login-hint');
    await expect(hint).toContainText('Входящий вебхук');
    await expect(hint).toContainText('Задачи');
    // Р15 обязан быть на экране: вебхук выпускается от имени ОДНОГО
    // сотрудника, и в remote-mode его переписку увидят все.
    await expect(hint).toContainText('одного сотрудника');

    // Кнопка отвязки недоступна, пока портал не подключён.
    await expect(page.locator('#b24-logout')).toBeHidden();
  });

  test('список диалогов и директория задач', async ({ page }) => {
    await mockBitrixEnabled(page, STATUS_AUTHORIZED);
    await page.goto('/');
    await openBitrixTab(page);
    await expect(page.locator('#b24-login')).toBeHidden({ timeout: 5_000 });

    await expect(page.locator('#b24-dialog-list .b24-dialog')).toHaveCount(3, { timeout: 10_000 });
    const state = await debugState(page);
    expect(state.loginState, 'портал подключён').toBe('authorized');
    expect(state.dialogs.loaded, 'загружены все три строки').toBe(3);
    expect(state.dialogs.visible, 'без фильтра видны все три').toBe(3);

    // Чат задачи помечен в списке: без метки он неотличим от обычного чата, а
    // кнопки в его карточке — рабочие.
    await expect(page.locator('#b24-dialog-list .b24-dialog-task')).toHaveCount(1);

    // Директория ~/tasks оставляет только чаты задач — нарезка ЛОКАЛЬНАЯ, без
    // запроса к порталу.
    await page.locator('#b24-dir-list .b24-dir[data-dir="tasks"]').click();
    await expect(page.locator('#b24-dialog-list .b24-dialog')).toHaveCount(1);
    const inTasks = await debugState(page);
    expect(inTasks.activeDir).toBe('tasks');

    await page.locator('#b24-dir-list .b24-dir[data-dir="all"]').click();
    await expect(page.locator('#b24-dialog-list .b24-dialog')).toHaveCount(3);
  });

  test('тема: режим меняет data-b24-theme, скриншоты обеих палитр', async ({ page }) => {
    await mockBitrixEnabled(page, STATUS_AUTHORIZED);
    await page.goto('/');
    await openBitrixTab(page);

    await setB24ThemeMode(page, 'dark');
    await expect(page.locator('#bitrix')).toHaveAttribute('data-b24-theme', 'dark');
    await expect(page.locator('#b24-theme-toggle')).toHaveAttribute('data-b24-theme-mode', 'dark');
    await page.locator('#bitrix').screenshot({ path: 'test-results/bitrix-dark.png' });

    await setB24ThemeMode(page, 'light');
    await expect(page.locator('#bitrix')).toHaveAttribute('data-b24-theme', 'light');
    await expect(page.locator('#b24-theme-toggle')).toHaveAttribute('data-b24-theme-mode', 'light');
    await page.locator('#bitrix').screenshot({ path: 'test-results/bitrix-light.png' });

    // Режим переживает перезагрузку страницы.
    await page.reload();
    await expect(page.locator('#bitrix-toggle')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('#bitrix')).toHaveAttribute('data-b24-theme', 'light');

    // Возвращаем auto: localStorage общий на контекст, а describe идёт serial.
    await setB24ThemeMode(page, 'auto');
  });

  test('настройки: вкладка Битрикс24 с видимостью и горячими клавишами', async ({ page }) => {
    await mockBitrixEnabled(page, STATUS_AUTHORIZED);
    await page.goto('/');
    await openBitrixTab(page);

    await page.evaluate(async () => {
      const mod = await import('/js/settings/modal.js');
      mod.openSettingsModal('bitrix');
    });

    const panel = page.locator('#ps-panel-bitrix');
    await expect(panel).toBeVisible({ timeout: 5_000 });
    // Раздел видимости: тумблер и список видов диалогов.
    await expect(panel.locator('.b24-filter-enabled')).toBeVisible({ timeout: 5_000 });
    await expect(panel.locator('.b24-visibility-kinds .b24-visibility-item')).toHaveCount(3);
    // Раздел клавиш: таблица действий (классы общей формы исторически tg-).
    await expect(panel.locator('.tg-hotkeys-row').first()).toBeVisible();
    await expect(panel).toContainText('Горячие клавиши Битрикс24');
    // Тумблер клавиш ВКЛЮЧЁН по умолчанию — дефолт настройки `true`, и это
    // отличается от тумблера видимости рядом.
    await expect(panel.locator('.tg-hotkeys-enabled')).toBeChecked();

    await page.locator('#ps-close').click();
    await expect(panel).toBeHidden();
  });

  test('белый список убирает диалог из колонки, счётчиков и бейджа', async ({ page }) => {
    await mockBitrixEnabled(page, STATUS_AUTHORIZED);
    await page.goto('/');
    await openBitrixTab(page);
    await expect(page.locator('#b24-dialog-list .b24-dialog')).toHaveCount(3, { timeout: 10_000 });

    // Бейдж до фильтра — серверный счётчик (9), а не сумма страницы.
    await expect(page.locator('#b24-unread-badge')).toHaveText('9', { timeout: 10_000 });

    // Оставляем ТОЛЬКО личные диалоги.
    await setUserSettings(page, {
      bitrix_filter_enabled: true,
      bitrix_visible_kinds: ['user'],
      bitrix_visible_peers: [],
    });

    await expect(page.locator('#b24-dialog-list .b24-dialog')).toHaveCount(1);
    const filtered = await debugState(page);
    expect(filtered.filter.active, 'фильтр действует').toBe(true);
    expect(filtered.dialogs.visible, 'колонка: видна одна строка').toBe(1);
    expect(filtered.dialogs.loaded, 'загруженные строки не потеряны').toBe(3);

    // Счётчик директории ~/tasks обнулился: чат задачи скрыт.
    await expect(page.locator('#b24-dir-tasks-count')).toHaveText('');
    await expect(page.locator('#b24-dir-all-count')).toHaveText('1');

    // Бейдж считается по ВИДИМЫМ строкам: серверные 9 игнорируются целиком,
    // потому что расщепить готовое число портала нечем.
    await expect(page.locator('#b24-unread-badge')).toHaveText('3');

    // Возврат: выключенный фильтр возвращает всё.
    await setUserSettings(page, { bitrix_filter_enabled: false });
    await expect(page.locator('#b24-dialog-list .b24-dialog')).toHaveCount(3);
    await expect(page.locator('#b24-unread-badge')).toHaveText('9');
  });

  test('владение клавиатурой: при открытом #bitrix «j» не двигает сайдбар сессий', async ({ page }) => {
    // Портал подключён: оверлей закрыт, иначе панель клавиатурой не владеет по
    // построению (в форме подключения клавиши принадлежат полю).
    await mockBitrixEnabled(page, STATUS_AUTHORIZED);
    await page.goto('/');
    await openBitrixTab(page);
    await expect(page.locator('#b24-login')).toBeHidden({ timeout: 5_000 });

    // Фокус нигде (на body) — панель считает клавиши своими.
    await page.evaluate(() => {
      const el = document.activeElement as HTMLElement | null;
      if (el && typeof el.blur === 'function') el.blur();
    });
    const owns = await page.evaluate(() => {
      const app = (window as unknown as { ForgeApp?: { b24Keys?: { owns?: () => boolean } } }).ForgeApp;
      return !!(app && app.b24Keys && typeof app.b24Keys.owns === 'function' && app.b24Keys.owns());
    });
    expect(owns, 'открытая панель Битрикс24 обязана владеть клавиатурой').toBe(true);

    // И она зарегистрирована в РЕЕСТРЕ владельцев клавиатуры — до третьей
    // панели имена были захардкожены в static/hotkeys.js.
    const registered = await page.evaluate(() => {
      const app = (window as unknown as { ForgeApp?: { keyboardOwners?: unknown } }).ForgeApp;
      const reg = app && app.keyboardOwners;
      return Array.isArray(reg) && reg.includes('b24Keys');
    });
    expect(registered, 'панель регистрирует себя в window.ForgeApp.keyboardOwners').toBe(true);

    await page.keyboard.press('j');
    await page.keyboard.press('k');
    await page.waitForTimeout(200);

    const active = await page.evaluate(() => {
      const el = document.activeElement as HTMLElement | null;
      return {
        isSessionItem: !!(el && el.classList && el.classList.contains('session-item')),
        insideB24: !!(el && document.getElementById('bitrix')?.contains(el)),
        tag: el ? el.tagName : null,
      };
    });
    expect(
      active.isSessionItem,
      'vim-навигация hotkeys.js обязана молчать, пока открыта панель мессенджера',
    ).toBe(false);
    expect(active.insideB24 || active.tag === 'BODY').toBe(true);

    // Уход фокусом в сайдбар возвращает клавиши vim'у — иначе открытая вкладка
    // отняла бы j/k у всего остального интерфейса.
    await page.evaluate(() => {
      const item = document.querySelector('#sidebar') as HTMLElement | null;
      if (item && typeof item.focus === 'function') {
        item.setAttribute('tabindex', '-1');
        item.focus();
      }
    });
    const ownsAfter = await page.evaluate(() => {
      const app = (window as unknown as { ForgeApp?: { b24Keys?: { owns?: () => boolean } } }).ForgeApp;
      return !!(app && app.b24Keys && typeof app.b24Keys.owns === 'function' && app.b24Keys.owns());
    });
    expect(ownsAfter, 'фокус вне панели — клавиши больше не её').toBe(false);
  });

  test('мобильная раскладка: колонка off-canvas и открывается по ☰', async ({ page }) => {
    await mockBitrixEnabled(page, STATUS_AUTHORIZED);
    await page.setViewportSize({ width: 420, height: 780 });
    await page.goto('/');
    await openBitrixTabViaJs(page);
    // Запас времени больше обычного: в полном прогоне спека идёт после тех, что
    // создают tmux-сессии, и сервер отвечает медленнее — на список это влияет
    // сильнее, чем на разметку.
    await expect(page.locator('#b24-dialog-list .b24-dialog')).toHaveCount(3, { timeout: 20_000 });

    const narrow = await debugState(page);
    expect(narrow.mobile.narrow, 'экран считается узким по тому же порогу, что и CSS').toBe(true);
    expect(narrow.mobile.maxWidth).toBe(768);
    expect(narrow.mobile.open, 'по умолчанию колонка уехала за край').toBe(false);

    // Кнопка ☰ видна только на узком экране.
    const burger = page.locator('#b24-contacts-toggle');
    await expect(burger).toBeVisible();
    await expect(burger).toHaveAttribute('aria-expanded', 'false');
    await burger.click();
    await expect(burger).toHaveAttribute('aria-expanded', 'true');
    const opened = await debugState(page);
    expect(opened.mobile.open, 'колонка выехала поверх ленты').toBe(true);

    // Выбор диалога закрывает колонку — ради этого её и открывали.
    await page.locator('#b24-dialog-list .b24-dialog').first().click();
    await expect(burger).toHaveAttribute('aria-expanded', 'false');
    const afterSelect = await debugState(page);
    expect(afterSelect.mobile.open, 'после выбора диалога колонка уехала').toBe(false);

    // На широком экране ☰ не показывается вовсе.
    await page.setViewportSize({ width: 1280, height: 800 });
    await expect(burger).toBeHidden();
  });

  test('три панели-мессенджера взаимоисключающи и не роняют консоль', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (e) => pageErrors.push(e.message));

    await mockBitrixEnabled(page, STATUS_AUTHORIZED);
    await page.goto('/');
    await expect(page.locator('#tab-bar')).toBeVisible({ timeout: 10_000 });
    await openBitrixTab(page);

    // Уходим на терминал и обратно: панель обязана пережить переключение.
    await page.locator('#tab-terminal').click();
    await expect(page.locator('#bitrix')).toBeHidden({ timeout: 5_000 });
    await page.locator('#bitrix-toggle').click();
    await expect(page.locator('#bitrix')).toBeVisible({ timeout: 5_000 });
    await expect(page.locator('#b24-prompt')).toBeVisible();

    const hidden = await page.evaluate(() => ({
      telegram: (document.getElementById('telegram') as HTMLElement).hidden,
      vk: (document.getElementById('vk') as HTMLElement).hidden,
      bitrix: (document.getElementById('bitrix') as HTMLElement).hidden,
    }));
    expect(hidden.telegram, 'одновременно открытыми панели быть не могут').toBe(true);
    expect(hidden.vk).toBe(true);
    expect(hidden.bitrix).toBe(false);

    expect(pageErrors, 'необработанных исключений быть не должно').toEqual([]);
  });
});
