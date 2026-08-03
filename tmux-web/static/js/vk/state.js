// tmux-web — VK plugin: клиентское состояние вкладки.
//
// Единственный мутируемый объект модуля (по образцу js/core/state.js):
// импортируется остальными модулями плагина и правится ими напрямую.
// Никакой реактивности — перерисовку инициирует тот, кто поменял поле.
//
// ============================================================================
// ЗАМОРОЖЕННЫЙ КОНТРАКТ ЛЕКСИКИ (задача Ф1.1)
// ============================================================================
//
// Структурная лексика UI остаётся ТЕЛЕГРАМНОЙ, VK-лексика живёт только на
// границе API и в именах полей DTO. Причина сугубо практическая: при
// дублировании кода главный инструмент сопровождения — `diff
// js/telegram/chat.js js/vk/chat.js`, и переименование ради «правильной»
// терминологии превратило бы осмысленный дифф в шум.
//
//   структурное имя в UI  |  что это у VK              |  где встречается
//   ----------------------|----------------------------|------------------
//   dialog                |  conversation (беседа)      |  списки, рендер
//   dialogId              |  peer_id (СТРОКОЙ)          |  ключи Map, роуты
//   dir                   |  серверный filter           |  колонка // FILTERS
//   messageId             |  message.id (i32)           |  лента, медиа
//   cmid                  |  conversation_message_id    |  ответы (Ф6)
//   media / attachments   |  attachment[] (до 10 штук)  |  карточки (Ф7)
//   community             |  сообщество, peer_id < 0    |  секция // COMMUNITIES
//
// Пути роутов — `/api/vk/dialogs`, а НЕ `/api/vk/conversations`: см. таблицу
// в шапке `plugins/vk/src/routes/mod.rs`, обе обязаны совпадать.
// Состояние «нет права messages» называется `no_messages_scope` (не
// `insufficient_scope`). Ключи настроек белого списка — `vk_visible_kinds` и
// `vk_visible_peers` (не `filters`/`dialogs`).
//
// ============================================================================
//
// Все id — СТРОКИ, ровно как их отдаёт бэкенд: `peer_id` беседы равен
// `2_000_000_000 + chat_id`, и в Number он ещё влезает, но контракт
// сознательно единый с телеграмным — иначе ключи Map начали бы расходиться по
// типу между двумя близнецами-модулями.

import { normalizeFilter } from './visibility.js';

export const vkState = {
    /**
     * Список бесед ВСЕГДА для `filter=all` — источник бейджа и белого списка.
     *
     * Отдельный от `dirDialogs` намеренно: у VK четыре «директории» это
     * СЕРВЕРНЫЕ фильтры (параметр `filter` метода messages.getConversations),
     * то есть другой запрос, а не подмножество загруженного списка. Держать
     * один список означало бы считать бейдж по случайной выборке.
     */
    dialogs: [],
    /** Транзиентный список выбранного серверного фильтра (кроме `all`). */
    dirDialogs: [],
    /**
     * Активный пункт секции // FILTERS: 'all' | 'unread' | 'important' |
     * 'unanswered'. Живёт в состоянии (а не в DOM), поэтому переживает
     * переключение вкладок devforge в рамках сессии.
     */
    activeDir: 'all',
    /** peer_id (строка) → массив MessageDto по возрастанию id. */
    messages: new Map(),
    /** Открытая беседа или null. */
    activeDialogId: null,
    /** upload_id → { file, sent, total, phase } — прогресс отправки файлов. */
    uploads: new Map(),
    /**
     * Ключ медиа → objectURL. Ключ УЖЕ С ИНДЕКСОМ вложения
     * (`peer/mid/idx/kind`): у сообщения VK до 10 вложений, и телеграмный ключ
     * `dialog/message.kind` их не различал бы — второе фото затирало бы первое
     * и в кэше, и в учёте objectURL.
     */
    mediaCache: new Map(),
    /**
     * Общий счётчик непрочитанного — источник бейджа на кнопке Ⓥ.
     *
     * У VK он приходит ГОТОВЫМ (событие LongPoll 80 → WS-кадр `unread_total`),
     * поэтому бейдж перестаёт быть суммой по загруженной странице списка.
     */
    unreadTotal: 0,
    /**
     * Активная captcha (VK код 14) или null: `{sid, img, pending}`.
     *
     * Живёт в состоянии, а не в модуле captcha.js, потому что её наличие
     * обязаны видеть двое: сам оверлей и клавиатурная навигация (при открытой
     * captcha панель НЕ владеет клавиатурой, Ф11).
     */
    captcha: null,
    /** Тема панели: 'auto' | 'dark' | 'light'. */
    themeMode: 'auto',
    /** Плагин включён на сервере (GET /api/vk/status ответил 200). */
    enabled: false,
    /** Последнее известное состояние логина (LoginStateDto.state). */
    loginState: 'not_configured',
    /** Данные авторизованного пользователя (VkUserDto) или null. */
    selfUser: null,
    /** Активное WebSocket-соединение или null. */
    ws: null,
    /** Последний измеренный RTT ping→pong, мс (для LAT в статусбаре). */
    lat: null,
};
//
// ЧЕГО В СОСТОЯНИИ НЕТ И НЕ БУДЕТ:
//   folders        — папок у VK нет, их место заняли 4 серверных фильтра;
//   notifyDefaults — глобальных категорийных дефолтов оповещений у VK нет,
//                    глушение только per-peer (`push_settings`), и `muted`
//                    приходит готовым в DialogDto и в кадре `new_message`.

/** Кэш разбора белого списка видимости: {key, filter}. */
const filterCache = { key: null, value: null };

/**
 * Действующий фильтр видимости бесед (белый список из настроек).
 *
 * Настройки читаются лениво через публичный контракт `window.ForgeApp`, тем же
 * приёмом, что в `keynav.js` и `hotkeys.js`: модули панели грузятся раньше,
 * чем приходит ответ `/api/user-settings`, и снимок, взятый на старте, застыл
 * бы навсегда. Отсутствие настроек означает «фильтр выключен» — это совпадает
 * и с backend-дефолтом, и с поведением до фичи.
 *
 * Результат кэшируется по СОДЕРЖИМОМУ настроек: функция зовётся на каждое
 * входящее сообщение, а пересобирать два Set'а ради каждого кадра незачем.
 * Ключ по содержимому (а не по ссылке) даёт нужное свойство: сохранённые
 * настройки действуют СРАЗУ, без перезагрузки страницы.
 *
 * Сам разбор живёт в `visibility.js` вместе с предикатами — здесь только
 * кэш: у нормализации нет ни состояния, ни зависимостей, и её обязан уметь
 * позвать напрямую тест и форма настроек.
 *
 * @returns {{enabled: boolean, active: boolean, kinds: Array<string>,
 *   peers: Array<string>, kindSet: Set<string>, peerSet: Set<string>}}
 */
export function currentVisibilityFilter() {
    const app = (typeof window !== 'undefined') ? window.ForgeApp : null;
    const us = (app && app.state && app.state.userSettings) || null;
    let key;
    try {
        key = JSON.stringify([
            us && us.vk_filter_enabled,
            us && us.vk_visible_kinds,
            us && us.vk_visible_peers,
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
 * Выбрасывает данные аккаунта: беседы, сообщения, загрузки и медиа.
 *
 * objectURL'ы ОБЯЗАТЕЛЬНО revoke'аются — иначе скачанные бинарники живут в
 * памяти страницы до перезагрузки, и после отвязки токена данные прошлого
 * аккаунта остаются доступны по ссылке.
 *
 * Поля логина (`loginState`, `selfUser`) НЕ трогаются: их выставляет
 * applyLoginState по ответу сервера, и он же вызывает эту функцию — сброс
 * затёр бы только что применённое состояние.
 */
export function clearVkAccountData() {
    for (const url of vkState.mediaCache.values()) {
        if (typeof url === 'string' && url.startsWith('blob:')) {
            try { URL.revokeObjectURL(url); } catch (_) { /* уже отозван */ }
        }
    }
    vkState.dialogs = [];
    vkState.dirDialogs = [];
    vkState.activeDir = 'all';
    vkState.messages.clear();
    vkState.activeDialogId = null;
    vkState.uploads.clear();
    vkState.mediaCache.clear();
    vkState.unreadTotal = 0;
    // Captcha принадлежит конкретной сессии: показывать картинку от прошлого
    // токена бессмысленно, а её `sid` уже не примут.
    vkState.captcha = null;
}

/** Сбрасывает состояние к исходному (используется при logout). */
export function resetVkState() {
    clearVkAccountData();
    vkState.loginState = 'not_configured';
    vkState.selfUser = null;
    vkState.lat = null;
}

/** Снимок состояния для e2e/диагностики. */
export function _debugVkState() {
    return {
        enabled: vkState.enabled,
        loginState: vkState.loginState,
        activeDir: vkState.activeDir,
        dialogs: vkState.dialogs.length,
        dirDialogs: vkState.dirDialogs.length,
        unreadTotal: vkState.unreadTotal,
        captcha: vkState.captcha ? { sid: vkState.captcha.sid } : null,
        themeMode: vkState.themeMode,
    };
}
