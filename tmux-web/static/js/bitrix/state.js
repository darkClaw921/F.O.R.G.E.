// tmux-web — Bitrix24 plugin: клиентское состояние вкладки.
//
// Единственный мутируемый объект модуля (по образцу js/core/state.js и
// js/vk/state.js): импортируется остальными модулями плагина и правится ими
// напрямую. Никакой реактивности — перерисовку инициирует тот, кто поменял
// поле.
//
// ============================================================================
// ЗАМОРОЖЕННЫЙ КОНТРАКТ ЛЕКСИКИ
// ============================================================================
//
// Структурная лексика UI остаётся ТЕЛЕГРАМНОЙ, лексика портала живёт только
// на границе API и в именах полей DTO. Причина сугубо практическая: при
// дублировании кода главный инструмент сопровождения — построчный дифф
// `js/vk/chat.js` ↔ `js/bitrix/chat.js`, и переименование ради «правильной»
// терминологии превратило бы осмысленный дифф в шум.
//
//   структурное имя в UI  |  что это у Битрикс24          |  где встречается
//   ----------------------|-------------------------------|------------------
//   dialog                |  чат / диалог / чат задачи     |  списки, рендер
//   dialogId              |  DIALOG_ID ("chat7"/"sg12"/"19")|  ключи Map, роуты
//   dir                   |  ЛОКАЛЬНАЯ директория списка   |  колонка // DIRS
//   messageId             |  message.id (i64)              |  лента, медиа
//   media / attachments   |  files[] сообщения             |  карточки (Ф8)
//   task                  |  задача (tasks.task.*)         |  карточка (Ф10)
//
// Пути роутов — `/api/bitrix/dialogs`, а НЕ `/api/bitrix/chats`: см. таблицу
// в шапке `plugins/bitrix/src/routes/mod.rs`, обе стороны обязаны совпадать
// дословно — расхождение даёт молчаливый 404, а не ошибку сборки.
//
// Состояние «у вебхука нет нужных прав» называется `scope_missing`. Ключи
// настроек белого списка (Ф11) — `bitrix_visible_kinds` и
// `bitrix_visible_peers`, а с Ф7.4 у каждого портала своя запись в карте
// `bitrix_portal_prefs` (правило чтения — в шапке `visibility.js`).
//
// ============================================================================
//
// Все id — СТРОКИ, ровно как их отдаёт бэкенд. Для Битрикс24 это не вопрос
// точности Number, а вопрос формы: `dialog_id` полиморфен у самого портала
// (число для личных диалогов, строка `"chat7"` для групповых), и единая
// строковая форма — единственный способ не разветвлять фронт.
//
// ЧЕТЫРЕ ДИРЕКТОРИИ — ЛОКАЛЬНЫЕ. Главное отличие от VK: там `all/unread/…`
// это СЕРВЕРНЫЙ параметр `filter`, то есть отдельный запрос и отдельный
// список. Здесь портал отдаёт ОДИН список (`im.recent.get`), а разделение
// чаты/задачи/непрочитанные выводится из полей строки (`entity_type`,
// `unread_count`). Поэтому второго списка вроде `dirDialogs` тут НЕТ и не
// будет — он был бы копией того же массива.

import { activePortal } from './api.js';
import { normalizeFilter, PORTAL_PREFS_KEY } from './visibility.js';

export const b24State = {
    /**
     * Единственный список диалогов — он же источник бейджа и белого списка.
     * Директории фильтруют его локально (см. шапку модуля).
     */
    dialogs: [],
    /**
     * Активная директория: 'all' | 'chats' | 'tasks' | 'unread'.
     * Живёт в состоянии (а не в DOM), поэтому переживает переключение вкладок
     * devforge в рамках сессии.
     */
    activeDir: 'all',
    /** dialog_id (строка) → массив MessageDto по возрастанию id. */
    messages: new Map(),
    /** Открытый диалог (строка) либо null. */
    activeDialogId: null,
    /**
     * task_id (строка) → карточка задачи (Ф10).
     *
     * Отдельная карта, а не поле диалога: у задачи бывает НЕ быть чата вовсе
     * (Р20), и вешать карточку на строку списка означало бы терять такие
     * задачи целиком.
     */
    tasks: new Map(),
    /** upload_id → {file, sent, total, phase} (Ф9). */
    uploads: new Map(),
    /**
     * Потолок размера отправляемого файла, байты (Р7).
     *
     * Приходит из `GET /api/bitrix/status` (`max_upload_bytes`), а НЕ зашит во
     * фронт: значение живёт в `bitrix.toml`, а скрипты браузер держит в кэше
     * PWA неделями — своя копия числа разъехалась бы с конфигом при первом же
     * его изменении. Ноль означает «сервер ещё не ответил»: клиентская
     * проверка тогда не отсекает ничего, и решение принимает сервер (413).
     */
    maxUploadBytes: 0,
    /** ключ кэша → objectURL (Ф8). Обязателен revoke при смене диалога. */
    mediaCache: new Map(),
    /**
     * Счётчики портала. Их ТРИ независимых плюс уведомления, и общий бейдж —
     * не сумма загруженной страницы, а серверное число.
     */
    counters: {
        total: 0,
        chat: 0,
        dialog: 0,
        tasks: 0,
        notify: 0,
        // ВТОРОЕ семейство (Ф10): состояние самих задач. Портал считает его
        // другим методом (`tasks.task.counters.get`) и обнуляет другим
        // действием, поэтому складывать его с `tasks` нельзя — сложенный
        // бейдж не обнулить ни чтением ленты, ни закрытием задачи.
        expired: 0,
        new_comments: 0,
        mentioned: 0,
    },
    /** Режим темы панели: 'auto' | 'dark' | 'light'. */
    themeMode: 'auto',
    /** GET /api/bitrix/status ответил 200 (плагин включён на сервере). */
    enabled: false,
    /**
     * Состояние подключения (см. LoginFlow::kind в plugins/bitrix/src/state.rs):
     * not_configured | checking | invalid_url | invalid_credentials |
     * scope_missing | throttled | disconnected | authorized.
     */
    loginState: 'not_configured',
    /** Владелец вебхука (Р15: он один на всех, кто открыл devforge). */
    selfUser: null,
    /** Живой сокет `/ws/bitrix` либо null. */
    ws: null,
    /** Оценка задержки по дрейфу ping'а, мс. */
    lat: null,
    /**
     * Портал душит интенсивность (Р16). Отдельный флаг, а не режим связи:
     * «медленно» и «оборвалось» лечатся по-разному, и статусбар обязан
     * показывать их разными метками.
     */
    throttled: false,
};

/**
 * Полный сброс данных аккаунта — вызывается при logout и при падении
 * состояния из `authorized`.
 *
 * `URL.revokeObjectURL` по всему медиа-кэшу ОБЯЗАТЕЛЕН: objectURL держит
 * блоб в памяти вкладки до явного отзыва, и «разлогинился и снова вошёл»
 * без него медленно съедает сотни мегабайт.
 */
export function clearB24AccountData() {
    b24State.dialogs = [];
    b24State.activeDir = 'all';
    b24State.messages = new Map();
    b24State.activeDialogId = null;
    b24State.tasks = new Map();
    b24State.uploads = new Map();
    for (const url of b24State.mediaCache.values()) {
        try { URL.revokeObjectURL(url); } catch (_) { /* уже отозван */ }
    }
    b24State.mediaCache = new Map();
    b24State.counters = {
        total: 0, chat: 0, dialog: 0, tasks: 0, notify: 0,
        expired: 0, new_comments: 0, mentioned: 0,
    };
    b24State.selfUser = null;
    b24State.throttled = false;
}

/** Кэш разбора белого списка ПО ПОРТАЛАМ: `portal_id` → {key, value}. */
const filterCache = new Map();

/**
 * Действующий белый список диалогов (Ф4/Ф11).
 *
 * Настройки читаются ЛЕНИВО через публичный контракт `window.ForgeApp`, тем же
 * приёмом, что у соседних панелей: модули грузятся раньше, чем приходит ответ
 * `/api/user-settings`, и снимок, взятый на старте, застыл бы навсегда.
 * Отсутствие настроек означает «фильтр выключен» — это совпадает и с
 * дефолтом бэкенда, и с поведением до появления фичи.
 *
 * Результат кэшируется по СОДЕРЖИМОМУ настроек, а не по ссылке на объект:
 * функция зовётся на каждый рендер колонки и на каждый входящий кадр, а форма
 * настроек пересобирает объект при каждом сохранении. Ключ по содержимому даёт
 * нужное свойство — сохранённые настройки действуют СРАЗУ, без перезагрузки.
 *
 * Сам разбор живёт в `visibility.js` вместе с предикатами: у нормализации нет
 * ни состояния, ни зависимостей, и её обязаны уметь позвать напрямую и тест, и
 * форма настроек.
 *
 * ПОРТАЛ (Ф7.4). Белый список у каждого портала свой, и спрашивать нужно
 * настройки ТОГО портала, чей кадр обрабатывается: кадры соседних порталов
 * приходят в тот же сокет, и общий фильтр прятал бы в одном портале чат,
 * отмеченный в другом. Умолчание — АКТИВНЫЙ портал панели (`api.js`, а не
 * `b24State`: `clearB24AccountData` обнуляет состояние ровно в момент
 * переключения портала, то есть стёр бы то, что как раз и меняется).
 *
 * Кэш — ПО ПОРТАЛАМ: одна ячейка означала бы, что чередование своих и чужих
 * кадров пересобирает разбор на каждом кадре, а именно на потоке кадров кэш и
 * нужен.
 *
 * @param {?string} [portalId] — портал; пусто ⇒ активный портал панели
 * @returns {object} результат `visibility.js::normalizeFilter`
 */
export function currentVisibilityFilter(portalId) {
    const app = (typeof window !== 'undefined') ? window.ForgeApp : null;
    const us = (app && app.state && app.state.userSettings) || null;
    const id = String(
        portalId == null || portalId === '' ? (activePortal() || '') : portalId,
    );
    let key;
    try {
        // Ключ по СОДЕРЖИМОМУ: форма настроек пересобирает объект настроек при
        // каждом сохранении, и ключ по ссылке держал бы старый разбор.
        key = JSON.stringify([
            us && us.bitrix_filter_enabled,
            us && us.bitrix_visible_kinds,
            us && us.bitrix_visible_peers,
            us && us[PORTAL_PREFS_KEY],
        ]);
    } catch (_) {
        key = null;
    }
    const hit = filterCache.get(id);
    if (key !== null && hit && hit.key === key && hit.value) {
        return hit.value;
    }
    const filter = normalizeFilter(us, id);
    filterCache.set(id, { key, value: filter });
    return filter;
}

/** Сбрасывает кэш фильтра — зовётся формой настроек сразу после сохранения. */
export function invalidateVisibilityFilter() {
    filterCache.clear();
}

/** Снимок состояния для e2e/диагностики. */
export function _debugB24State() {
    return {
        enabled: b24State.enabled,
        loginState: b24State.loginState,
        activeDir: b24State.activeDir,
        activeDialogId: b24State.activeDialogId,
        dialogs: b24State.dialogs.length,
        messages: b24State.messages.size,
        tasks: b24State.tasks.size,
        uploads: b24State.uploads.size,
        mediaCache: b24State.mediaCache.size,
        counters: { ...b24State.counters },
        themeMode: b24State.themeMode,
        throttled: b24State.throttled,
        lat: b24State.lat,
        hasSelfUser: !!b24State.selfUser,
    };
}
