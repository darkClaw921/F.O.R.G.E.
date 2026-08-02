// tmux-web — Telegram plugin: клиентское состояние вкладки.
//
// Единственный мутируемый объект модуля (по образцу js/core/state.js):
// импортируется остальными модулями плагина и правится ими напрямую.
// Никакой реактивности — перерисовку инициирует тот, кто поменял поле.
//
// Все id — СТРОКИ, ровно как их отдаёт бэкенд: 64-битные идентификаторы
// Telegram не помещаются в Number без потери точности, поэтому
// сравнивать/использовать как ключи Map их можно только строками.

import { normalizeFilter } from './visibility.js';

export const tgState = {
    /** Список диалогов (DialogDto), как пришёл с сервера. */
    dialogs: [],
    /** Пользовательские папки (GET /api/telegram/folders), порядок Telegram. */
    folders: [],
    /**
     * Активный пункт секции // DIRECTORIES: 'inbox' | 'archive' |
     * 'folder:<id>'. Живёт в состоянии (а не в DOM), поэтому переживает
     * переключение вкладок devforge в рамках сессии.
     */
    activeDir: 'inbox',
    /** dialog_id (строка) → массив MessageDto по возрастанию id. */
    messages: new Map(),
    /** Открытый диалог или null. */
    activeDialogId: null,
    /** upload_id → { file, sent, total, phase } — прогресс отправки файлов. */
    uploads: new Map(),
    /** ключ медиа → objectURL; при смене диалога URL'ы обязательно revoke'аются. */
    mediaCache: new Map(),
    /** Суммарное число непрочитанных — источник бейджа на кнопке ✈. */
    unreadTotal: 0,
    /**
     * Глобальные дефолты уведомлений по категориям Telegram (WS-кадр
     * `notify_defaults_changed`): `true` ⇒ вся категория заглушена.
     *
     * Именно этими настройками пользователь глушит «все личные чаты» или «все
     * каналы» с телефона. Разрешённый `muted` каждого диалога считает СЕРВЕР
     * (в DialogDto и в кадре new_message приходит уже готовое значение),
     * поэтому здесь дефолты лежат как последнее известное состояние: по их
     * смене вкладка перечитывает список диалогов и пересобирает бейдж.
     */
    notifyDefaults: { users: false, chats: false, broadcasts: false },
    /** Тема панели: 'auto' | 'dark' | 'light'. */
    themeMode: 'auto',
    /** Плагин включён на сервере (GET /api/telegram/status ответил 200). */
    enabled: false,
    /** Последнее известное состояние логина (LoginStateDto.state). */
    loginState: 'not_configured',
    /** Данные авторизованного пользователя (TgUserDto) или null. */
    selfUser: null,
    /** Активное WebSocket-соединение или null. */
    ws: null,
    /** Последний измеренный RTT ping→pong, мс (для LAT в статусбаре). */
    lat: null,
};

/** Кэш разбора белого списка видимости: {key, filter}. */
const filterCache = { key: null, value: null };

/**
 * Действующий фильтр видимости чатов (белый список из настроек).
 *
 * Настройки читаются лениво через публичный контракт `window.ForgeApp`, тем же
 * приёмом, что в `keynav.js` и `hotkeys.js`: модули панели грузятся раньше,
 * чем приходит ответ `/api/user-settings`, и снимок, взятый на старте, застыл
 * бы навсегда. Отсутствие настроек означает «фильтр выключен» — это совпадает
 * и с backend-дефолтом, и с поведением до фичи.
 *
 * Результат кэшируется по СОДЕРЖИМОМУ настроек: `dialogRings` зовёт эту
 * функцию на каждое входящее сообщение, а пересобирать два Set'а ради каждого
 * кадра незачем. Ключ по содержимому (а не по ссылке) даёт нужное свойство:
 * сохранённые настройки действуют сразу, без перезагрузки страницы.
 *
 * @returns {object} результат `visibility.js::normalizeFilter`
 */
export function currentVisibilityFilter() {
    const app = (typeof window !== 'undefined') ? window.ForgeApp : null;
    const us = (app && app.state && app.state.userSettings) || null;
    let key;
    try {
        key = JSON.stringify([
            us && us.telegram_filter_enabled,
            us && us.telegram_visible_folders,
            us && us.telegram_visible_dialogs,
        ]);
    } catch (_) {
        key = null;
    }
    if (key !== null && key === filterCache.key && filterCache.value) {
        return filterCache.value;
    }
    const filter = normalizeFilter(us);
    filterCache.key = key;
    filterCache.value = filter;
    return filter;
}

/** Сбрасывает кэш фильтра (после сохранения настроек). */
export function invalidateVisibilityFilter() {
    filterCache.key = null;
    filterCache.value = null;
}

/**
 * Выбрасывает данные аккаунта: диалоги, сообщения, загрузки и медиа.
 *
 * objectURL'ы ОБЯЗАТЕЛЬНО revoke'аются — иначе скачанные бинарники живут
 * в памяти страницы до перезагрузки, и после logout данные прошлого
 * аккаунта остаются доступны по ссылке.
 *
 * Поля логина (`loginState`, `selfUser`) НЕ трогаются: их выставляет
 * applyLoginState по ответу сервера, и он же вызывает эту функцию — сброс
 * затёр бы только что применённое состояние.
 */
export function clearTgAccountData() {
    for (const url of tgState.mediaCache.values()) {
        if (typeof url === 'string' && url.startsWith('blob:')) {
            try { URL.revokeObjectURL(url); } catch (_) { /* уже отозван */ }
        }
    }
    tgState.dialogs = [];
    tgState.folders = [];
    tgState.activeDir = 'inbox';
    tgState.messages.clear();
    tgState.activeDialogId = null;
    tgState.uploads.clear();
    tgState.mediaCache.clear();
    tgState.unreadTotal = 0;
    // Настройки уведомлений принадлежат аккаунту: чужие дефолты после входа в
    // другой — прямая дезинформация (тосты либо пропадут, либо польются).
    tgState.notifyDefaults = { users: false, chats: false, broadcasts: false };
}

/** Сбрасывает состояние к исходному (используется при logout). */
export function resetTgState() {
    clearTgAccountData();
    tgState.loginState = 'not_configured';
    tgState.selfUser = null;
    tgState.lat = null;
}
