// tmux-web — Bitrix24 plugin: REST-клиент.
//
// Тонкие обёртки поверх apiFetch (core/api.js) через общий разборщик конверта
// js/messenger/http-envelope.js. Authorization подставляет подменённый
// window.fetch из core/auth.js — здесь заголовков нет вовсе.
//
// ============================================================================
// ПУТИ — ЗАМОРОЖЕННЫЙ КОНТРАКТ
// ============================================================================
//
// Таблица обязана совпадать ДОСЛОВНО с шапкой plugins/bitrix/src/routes/mod.rs:
// расхождение даёт молчаливый 404, а не ошибку сборки. Сверку делает
// tmux-web/tests/frontend/bitrix_contract.test.mjs.
//
//   GET  /api/bitrix/status                       getStatus            (Ф1)
//   POST /api/bitrix/login/webhook                loginWebhook         (Ф3)
//   POST /api/bitrix/logout                       logout               (Ф3)
//   GET  /api/bitrix/filters                      getFilters           (Ф4)
//   GET  /api/bitrix/dialogs                      getDialogs           (Ф4)
//   GET  /api/bitrix/dialogs/:id/messages         getMessages          (Ф5)
//   GET  /api/bitrix/messages/:mid                getMessage           (Ф5)
//   POST /api/bitrix/dialogs/:id/messages         sendMessage          (Ф6)
//   PATCH  /api/bitrix/messages/:mid              editMessage          (Ф6)
//   DELETE /api/bitrix/messages/:mid              deleteMessage        (Ф6)
//   POST /api/bitrix/messages/:mid/like           likeMessage          (Ф6)
//   POST /api/bitrix/messages/:mid/command        sendCommand          (forge-zw3d)
//   POST /api/bitrix/dialogs/:id/read             markRead             (Ф6)
//   POST /api/bitrix/dialogs/:id/unread           markUnread           (Ф6)
//   POST /api/bitrix/dialogs/:id/typing           sendTyping           (Ф6)
//   POST /api/bitrix/dialogs/:id/pin              pinDialog            (Ф6)
//   POST /api/bitrix/dialogs/:id/hide             hideDialog           (Ф6)
//   GET  /api/bitrix/tasks                        getTasks             (Ф10)
//   GET  /api/bitrix/tasks/counters               getTaskCounters      (Ф10)
//   GET  /api/bitrix/tasks/:id                    getTask              (Ф10)
//   POST /api/bitrix/tasks/:id/:action            taskAction           (Ф10)
//   GET  /api/bitrix/media/:dialog/:mid/:index    mediaUrl             (Ф8)
//   GET  /api/bitrix/media/avatar/:user_id        avatarUrl            (Ф8)
//   POST /api/bitrix/upload                       uploadFile           (Ф9)
//   GET    /api/bitrix/dialogs/:id/prompt            getChatPrompt     (Ф12)
//   PUT    /api/bitrix/dialogs/:id/prompt            putChatPrompt     (Ф12)
//   GET    /api/bitrix/dialogs/:id/analysis          getSavedAnalysis  (Ф12)
//   POST   /api/bitrix/dialogs/:id/analysis          startAnalysis     (Ф12)
//   DELETE /api/bitrix/dialogs/:id/analysis          clearSavedAnalysis(Ф12)
//   POST   /api/bitrix/dialogs/:id/analysis/cancel   cancelAnalysis    (Ф12)
//   DELETE /api/bitrix/dialogs/:id/analysis/thread   clearThread       (Ф12)
//   POST   /api/bitrix/voice/:dialog/:mid/:index/transcribe  transcribeVoice    (ГС-Ф3)
//   GET    /api/bitrix/voice/:dialog/:mid/:index/transcript  getVoiceTranscript (ГС-Ф3)
//   GET    /api/bitrix/voice/settings                         getVoiceSettings   (ГС-Ф4)
//   PUT    /api/bitrix/voice/settings                         setVoiceSettings   (ГС-Ф4)
//
// Реализованы обёртки только для смонтированных роутов; остальные строки
// таблицы — карта на будущее, чтобы имена не расползлись.
//
// ============================================================================
// ТРИ ПРАВИЛА, КОТОРЫЕ ЛЕГКО ПОТЕРЯТЬ
// ============================================================================
//
// 1. Решения принимаются по `err.code`, а НЕ по тексту ошибки: тексты
//    меняются (в них подмешивается дословная формулировка портала), коды —
//    контракт, зафиксированный таблицей в plugins/bitrix/src/routes/error.rs.
// 2. У `loginWebhook` автоповторов НЕТ ни при каких условиях. Портал душит
//    интенсивность (Р16) отдельным состоянием `throttled`, и автоповтор его
//    только продлевает — повтор всегда инициирует пользователь.
// 3. Модуль импортируется потребителями как `import * as b24Api`: тогда
//    закэшированная старая версия даёт `undefined is not a function` в одном
//    месте, а не SyntaxError, роняющий всю панель.
//
// ИНВАРИАНТ СЕКРЕТА: вебхук — это ВЕСЬ URL целиком. Он проходит через этот
// модуль ровно один раз, телом POST'а, и нигде не задерживается: ни в
// localStorage/sessionStorage, ни в переменной модуля, ни в URL запроса
// (query-строка оседает в логах прокси). Разбор трёх форм ввода живёт ТОЛЬКО
// на бэкенде — второй парсер неизбежно разъехался бы с первым.

import { apiFetch } from '../core/api.js';
import { createCaller, jsonInit, parseErrorEnvelope } from '../messenger/http-envelope.js';

/** Единственная точка выхода модуля в сеть. */
const call = createCaller(apiFetch);

/** Пути роутов — единственное место, где они записаны во фронте. */
export const PATHS = {
    status: '/api/bitrix/status',
    loginWebhook: '/api/bitrix/login/webhook',
    logout: '/api/bitrix/logout',
    filters: '/api/bitrix/filters',
    dialogs: '/api/bitrix/dialogs',
    // ШАБЛОНЫ, а не готовые адреса: сегменты подставляет `withSeg`, и тогда
    // строка здесь дословно совпадает со строкой таблицы в routes/mod.rs —
    // именно это и сверяет bitrix_contract.test.mjs.
    dialogMessages: '/api/bitrix/dialogs/:id/messages',
    message: '/api/bitrix/messages/:mid',
    messageLike: '/api/bitrix/messages/:mid/like',
    messageCommand: '/api/bitrix/messages/:mid/command',
    dialogRead: '/api/bitrix/dialogs/:id/read',
    dialogUnread: '/api/bitrix/dialogs/:id/unread',
    dialogTyping: '/api/bitrix/dialogs/:id/typing',
    dialogPin: '/api/bitrix/dialogs/:id/pin',
    dialogHide: '/api/bitrix/dialogs/:id/hide',
    // Задачи (Ф10). `tasksCounters` объявлен ВЫШЕ `task`, потому что в
    // таблице роутов статический сегмент `counters` стоит рядом с `:id`, и
    // порядок здесь — напоминание о том, что это разные адреса, а не один.
    tasks: '/api/bitrix/tasks',
    tasksCounters: '/api/bitrix/tasks/counters',
    task: '/api/bitrix/tasks/:id',
    taskAction: '/api/bitrix/tasks/:id/:action',
    mediaFile: '/api/bitrix/media/:dialog/:mid/:index',
    mediaAvatar: '/api/bitrix/media/avatar/:user_id',
    // Приём файла в staging (Ф9). Ходит по нему НЕ этот модуль, а
    // `js/bitrix/upload.js` через XMLHttpRequest: у `fetch` нет прогресса
    // отправки. Путь всё равно записан здесь — иначе в проекте появилось бы
    // второе место с адресами плагина, и страж контракта сверял бы только одно
    // из них.
    upload: '/api/bitrix/upload',
    // Анализ чата через Claude (Ф12). Шаблоны, как и остальные адреса с
    // сегментом: подстановкой занимается `withSeg`, а строка здесь дословно
    // совпадает со строкой таблицы в routes/mod.rs — это и сверяет
    // bitrix_contract.test.mjs.
    dialogPrompt: '/api/bitrix/dialogs/:id/prompt',
    dialogAnalysis: '/api/bitrix/dialogs/:id/analysis',
    dialogAnalysisCancel: '/api/bitrix/dialogs/:id/analysis/cancel',
    dialogAnalysisThread: '/api/bitrix/dialogs/:id/analysis/thread',
    // Голосовые сообщения (ГС-Ф3). Адреса и формы ответов совпадают с VK и
    // Telegram: текст под голосовым рисует один общий модуль, и расхождение в
    // транспорте означало бы «в VK работает, в Битриксе нет».
    voiceTranscribe: '/api/bitrix/voice/:dialog/:mid/:index/transcribe',
    voiceTranscript: '/api/bitrix/voice/:dialog/:mid/:index/transcript',
    voiceSettings: '/api/bitrix/voice/settings',
};

/**
 * `GET /api/bitrix/status` — снимок состояния плагина.
 *
 * 404 означает «плагин выключен» (запущен devforge без `--bitrix`) и штатно
 * приводит к ошибке, которую гейт кнопки вкладки глотает.
 *
 * @returns {Promise<object>} StatusDto
 */
export function getStatus() {
    return call(PATHS.status);
}

/**
 * `POST /api/bitrix/login/webhook` — подключить портал.
 *
 * ТЕЛО СОДЕРЖИТ СЕКРЕТ. Значение приходит аргументом, никуда не копируется и
 * после вызова живёт только в поле ввода — которое очищается сразу после
 * успеха (см. js/bitrix/login.js).
 *
 * Принимаются три формы: полный адрес с именем метода, адрес без метода, с
 * хвостовым слэшем и без. Разбирает их СЕРВЕР.
 *
 * @param {string} webhook — то, что вставил пользователь
 * @returns {Promise<object>} LoginStateDto
 */
export function loginWebhook(webhook) {
    return call(PATHS.loginWebhook, jsonInit('POST', { webhook: String(webhook || '') }));
}

/**
 * `POST /api/bitrix/logout` — отвязать портал.
 *
 * Стирает вебхук ТОЛЬКО в этом devforge: отозвать входящий вебхук через REST
 * невозможно, на портале он продолжает действовать. Об этом обязан сказать UI
 * (текст приходит с сервера полем `logout_hint` в `/status`).
 *
 * @returns {Promise<object>} LoginStateDto
 */
export function logout() {
    return call(PATHS.logout, jsonInit('POST', {}));
}

/**
 * `GET /api/bitrix/filters` — директории колонки и счётчики непрочитанного.
 *
 * Директорий четыре и они статичны (`~/all`, `~/chats`, `~/tasks`,
 * `~/unread` — Р2); переменная часть — счётчики. После холодного старта
 * сервер отвечает из снимка, не обращаясь к порталу (Р3), поэтому вызов
 * дёшев и его не нужно экономить.
 *
 * @returns {Promise<object>} `{filters: [{key,title,unread}], counters: {...}}`
 */
export function getFilters() {
    return call(PATHS.filters);
}

/**
 * `GET /api/bitrix/dialogs?limit&offset&filter` — страница списка диалогов.
 *
 * Параметры собираются здесь, а не строкой у вызывающего: `URLSearchParams`
 * кодирует значения, а ручная склейка рано или поздно уедет с неэкранированным
 * значением директории.
 *
 * @param {{limit?: number, offset?: number, filter?: string}} [params]
 * @returns {Promise<object>} `{dialogs: DialogDto[], has_more: boolean}`
 */
export function getDialogs(params = {}) {
    const q = new URLSearchParams();
    if (Number.isFinite(params.limit)) q.set('limit', String(Math.trunc(params.limit)));
    if (Number.isFinite(params.offset)) q.set('offset', String(Math.trunc(params.offset)));
    if (params.filter) q.set('filter', String(params.filter));
    const qs = q.toString();
    return call(qs ? `${PATHS.dialogs}?${qs}` : PATHS.dialogs);
}

/** Кодирует сегмент пути: `dialog_id` — это чужая строка (`chat7`, `19`, `sg12`). */
function seg(value) {
    return encodeURIComponent(String(value == null ? '' : value));
}

/**
 * Подставляет значение в шаблон пути (`:id`, `:mid`).
 *
 * @param {string} template
 * @param {string} placeholder
 * @param {*} value
 * @returns {string}
 */
function withSeg(template, placeholder, value) {
    return template.replace(placeholder, seg(value));
}

/**
 * `GET /api/bitrix/dialogs/:id/messages?limit&before_id&after_id` — страница
 * истории.
 *
 * Направления взаимоисключающие: `beforeId` листает ВВЕРХ (строго старее),
 * `afterId` догружает НИЗ (строго новее). Сервер отдаёт страницу ВСЕГДА по
 * возрастанию `id`, каким бы порядком её ни прислал портал, — фронт про два
 * порядка портала не знает вовсе.
 *
 * `limit` сервер клампит потолком платформы (50) молча: просить больше можно,
 * получить — нельзя.
 *
 * @param {string} dialogId
 * @param {{limit?: number, beforeId?: number, afterId?: number,
 *   signal?: object}} [opts]
 * @returns {Promise<object>} `{messages: MessageDto[], has_more: boolean}`
 */
export function getMessages(dialogId, opts = {}) {
    const q = new URLSearchParams();
    if (Number.isFinite(opts.limit) && opts.limit > 0) q.set('limit', String(Math.trunc(opts.limit)));
    if (Number.isFinite(opts.beforeId) && opts.beforeId > 0) q.set('before_id', String(Math.trunc(opts.beforeId)));
    else if (Number.isFinite(opts.afterId) && opts.afterId > 0) q.set('after_id', String(Math.trunc(opts.afterId)));
    const qs = q.toString();
    const path = withSeg(PATHS.dialogMessages, ':id', dialogId) + (qs ? `?${qs}` : '');
    return call(path, opts.signal ? { signal: opts.signal } : undefined);
}

/**
 * `GET /api/bitrix/messages/:mid?dialog_id=…` — одно сообщение.
 *
 * Диалог обязателен: метода «дай сообщение по идентификатору» у портала нет
 * вовсе, и сервер достаёт его страницей на одну строку. Нужен догрузке
 * realtime (Ф7) — перезагружать из-за одного события всю ленту расточительно.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @returns {Promise<object>} MessageDto
 */
export function getMessage(dialogId, messageId) {
    return call(`${withSeg(PATHS.message, ':mid', messageId)}?dialog_id=${seg(dialogId)}`);
}

/**
 * `POST /api/bitrix/dialogs/:id/messages` — отправить текст.
 *
 * АВТОПОВТОРОВ НЕТ И БЫТЬ НЕ МОЖЕТ (Р9): у `im.message.add` нет
 * дедуп-идентификатора, поэтому повтор после обрыва — это второе сообщение у
 * собеседника, а не то же самое. Транспортный отказ приезжает как `503
 * not_connected`, и повтор инициирует ТОЛЬКО пользователь кнопкой на строке.
 *
 * Цитата ответа (Р10) собирается на сервере из `replyAuthor`/`replyText`:
 * формат обязан быть один на все вкладки и все версии кэшированных модулей.
 *
 * `uploadId` (Ф9) превращает вызов в отправку ФАЙЛА: сервер зовёт
 * `im.v2.File.upload`, который заливает файл и отправляет сообщение одним
 * вызовом, а `text` становится подписью и может быть пустым. Отдельного роута
 * у файла нет намеренно — иначе у отправки было бы два пути с двумя копиями
 * дедупа и рассылки. Идентификатор ОДНОРАЗОВЫЙ: сервер потребляет его до
 * заливки, поэтому повтор с тем же значением даёт `409 invalid_state`, а не
 * второй файл в чате.
 *
 * @param {string} dialogId
 * @param {{text: string, replyTo?: ?number, replyAuthor?: ?string,
 *   replyText?: ?string, uploadId?: ?string}} payload
 * @returns {Promise<object>} `{id, message, echoed}`
 */
export function sendMessage(dialogId, payload = {}) {
    const body = { text: String(payload.text == null ? '' : payload.text) };
    if (Number.isFinite(payload.replyTo) && payload.replyTo > 0) {
        body.reply_to = Math.trunc(payload.replyTo);
        if (payload.replyAuthor) body.reply_author = String(payload.replyAuthor);
        if (payload.replyText) body.reply_text = String(payload.replyText);
    }
    if (payload.uploadId) body.upload_id = String(payload.uploadId);
    return call(withSeg(PATHS.dialogMessages, ':id', dialogId), jsonInit('POST', body));
}

/**
 * `PATCH /api/bitrix/messages/:mid` — изменить текст сообщения.
 *
 * Пустой текст сервер ОТВЕРГАЕТ (400): у портала пустой `MESSAGE` удаляет
 * сообщение, а удаление живёт на своей кнопке с подтверждением.
 *
 * `dialogId` обязателен — им сервер перечитывает строку после правки (метку
 * `edited` ставит портал, и подделать её нечем).
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @param {string} text
 * @returns {Promise<object>} `{id, message, echoed}`
 */
export function editMessage(dialogId, messageId, text) {
    return call(
        withSeg(PATHS.message, ':mid', messageId),
        jsonInit('PATCH', { text: String(text == null ? '' : text), dialog_id: String(dialogId) }),
    );
}

/**
 * `DELETE /api/bitrix/messages/:mid` — удалить сообщение.
 *
 * Диалог не нужен: портал адресует метод одним идентификатором сообщения.
 *
 * @param {number|string} messageId
 * @returns {Promise<object>} `{ok, sent}`
 */
export function deleteMessage(messageId) {
    return call(withSeg(PATHS.message, ':mid', messageId), { method: 'DELETE' });
}

/**
 * `POST /api/bitrix/messages/:mid/like` — «лайк».
 *
 * ЕДИНСТВЕННАЯ доступная вебхуку реакция: эмодзи у платформы нет вовсе, и UI
 * не должен предлагать выбор, которого не существует. Метод переключающий —
 * `action` по умолчанию `auto`.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @param {?string} [action] — `auto` | `plus` | `minus`
 * @returns {Promise<object>} `{id, message, echoed}`
 */
export function likeMessage(dialogId, messageId, action) {
    const body = { dialog_id: String(dialogId) };
    if (action) body.action = String(action);
    return call(withSeg(PATHS.messageLike, ':mid', messageId), jsonInit('POST', body));
}

/**
 * `POST /api/bitrix/messages/:mid/command` — выполнить команду бота (кнопка
 * клавиатуры вида `COMMAND`, forge-zw3d).
 *
 * Поля дословно те, что уже приехали фронту в `ButtonDto` — сервер их не
 * запоминает по сообщению, поэтому кнопка отдаёт их обратно.
 *
 * @param {number|string} messageId
 * @param {{botId: number, command: string, commandParams?: ?string}} payload
 * @returns {Promise<object>} `{ok, sent}`
 */
export function sendCommand(messageId, payload) {
    const body = { bot_id: Number(payload.botId), command: String(payload.command || '') };
    if (payload.commandParams) body.command_params = String(payload.commandParams);
    return call(withSeg(PATHS.messageCommand, ':mid', messageId), jsonInit('POST', body));
}

/**
 * `POST /api/bitrix/dialogs/:id/read` — отметить прочитанным.
 *
 * Сервер троттлит вызов окном 800 мс на диалог и отвечает `{sent: false}` на
 * подавленные — это НЕ ошибка. Фронт троттлит своим таймером тоже (незачем
 * гонять сеть впустую), но полагаться только на него нельзя: панель бывает
 * открыта в нескольких вкладках.
 *
 * @param {string} dialogId
 * @param {?number} [messageId] — граница; без неё читается весь диалог
 * @returns {Promise<object>} `{ok, sent}`
 */
export function markRead(dialogId, messageId) {
    const body = {};
    if (Number.isFinite(messageId) && messageId > 0) body.message_id = Math.trunc(messageId);
    return call(withSeg(PATHS.dialogRead, ':id', dialogId), jsonInit('POST', body));
}

/**
 * `POST /api/bitrix/dialogs/:id/unread` — пометить непрочитанным.
 *
 * `messageId` ОБЯЗАТЕЛЕН: портал отвергает вызов без него, и сервер до сети
 * такой запрос не пропускает (400 `invalid_input`).
 *
 * @param {string} dialogId
 * @param {number} messageId
 * @returns {Promise<object>} `{ok, sent}`
 */
export function markUnread(dialogId, messageId) {
    return call(
        withSeg(PATHS.dialogUnread, ':id', dialogId),
        jsonInit('POST', { message_id: Math.trunc(Number(messageId) || 0) }),
    );
}

/**
 * `POST /api/bitrix/dialogs/:id/typing` — «печатает…».
 *
 * Односторонний индикатор: ВХОДЯЩИХ событий набора текста платформа вебхуку не
 * присылает, поэтому чужое «печатает» в панели не появляется никогда.
 *
 * @param {string} dialogId
 * @returns {Promise<object>} `{ok, sent}`
 */
export function sendTyping(dialogId) {
    return call(withSeg(PATHS.dialogTyping, ':id', dialogId), jsonInit('POST', {}));
}

/**
 * `POST /api/bitrix/dialogs/:id/pin` — закрепить или открепить диалог (Р14).
 *
 * @param {string} dialogId
 * @param {boolean} [pinned]
 * @returns {Promise<object>} `{ok, sent}`
 */
export function pinDialog(dialogId, pinned = true) {
    return call(withSeg(PATHS.dialogPin, ':id', dialogId), jsonInit('POST', { pinned: !!pinned }));
}

/**
 * `POST /api/bitrix/dialogs/:id/hide` — скрыть диалог из списка (Р14).
 *
 * Обратной операции у портала нет: диалог вернётся сам, от первого же нового
 * сообщения. Поэтому подтверждение обязательно.
 *
 * @param {string} dialogId
 * @returns {Promise<object>} `{ok, sent}`
 */
export function hideDialog(dialogId) {
    return call(withSeg(PATHS.dialogHide, ':id', dialogId), jsonInit('POST', {}));
}

// ---------------------------------------------------------------------------
// Задачи (Ф10)
// ---------------------------------------------------------------------------

/**
 * `GET /api/bitrix/tasks?filter&start` — страница задач.
 *
 * Страница ФИКСИРОВАНА платформой: пятьдесят записей, листается смещением
 * `start = (N-1)*50`. Параметра `limit` нет вовсе — просить его бессмысленно.
 *
 * `filter` — пресет (`all|mine|active|expired|closed`), а не сырой фильтр
 * портала: у того есть псевдополе `STATUS`, которое молча возвращает НОЛЬ
 * записей, и подпускать фронт к сборке фильтра означало бы однажды получить
 * пустую директорию без единой ошибки.
 *
 * @param {{filter?: string, start?: number, responsible?: number,
 *   group?: number}} [params]
 * @returns {Promise<object>} `{tasks: TaskDto[], total, start, has_more, filter}`
 */
export function getTasks(params = {}) {
    const q = new URLSearchParams();
    if (params.filter) q.set('filter', String(params.filter));
    if (Number.isFinite(params.start) && params.start > 0) q.set('start', String(Math.trunc(params.start)));
    if (Number.isFinite(params.responsible) && params.responsible > 0) {
        q.set('responsible', String(Math.trunc(params.responsible)));
    }
    if (Number.isFinite(params.group) && params.group >= 0) q.set('group', String(Math.trunc(params.group)));
    const qs = q.toString();
    return call(qs ? `${PATHS.tasks}?${qs}` : PATHS.tasks);
}

/**
 * `GET /api/bitrix/tasks/:id` — карточка задачи.
 *
 * Несёт `dialog_id` ленты, срок, статус и — главное — `actions`: набор
 * действий, разрешённых порталом ПРЯМО СЕЙЧАС. Он динамичен и меняется вместе
 * со статусом, поэтому карточка перезапрашивается после каждого действия.
 *
 * `dialog_id === null` означает «у задачи ещё нет чата» (Р20) — ветка
 * обязательная, а не редкая.
 *
 * @param {number|string} taskId
 * @returns {Promise<object>} TaskDto
 */
export function getTask(taskId) {
    return call(withSeg(PATHS.task, ':id', taskId));
}

/**
 * `GET /api/bitrix/tasks/counters` — счётчики задач.
 *
 * ДВА независимых семейства одним ответом: состояние задач (`expired`,
 * `new_comments`, `mentioned`) и непрочитанные СООБЩЕНИЯ чатов задач
 * (`unread`). Складывать их нельзя: сложенный бейдж не обнулить ни чтением
 * ленты, ни закрытием задачи.
 *
 * @returns {Promise<object>} `{expired, new_comments, mentioned, unread}`
 */
export function getTaskCounters() {
    return call(PATHS.tasksCounters);
}

/**
 * `POST /api/bitrix/tasks/:id/:action` — действие над задачей.
 *
 * Ключ действия берётся ИЗ КАРТОЧКИ (`task.actions`), а не из списка во
 * фронте: матрица прав динамична и приезжает с портала. Действие, которого в
 * матрице нет, сервер отвергает `409 invalid_state` — это НЕ сбой, а признак
 * того, что кнопка на экране устарела (статус мог поменяться в чужом
 * браузере), и карточку надо перечитать.
 *
 * Ответ несёт СВЕЖУЮ карточку (матрица уже другая) и `dialog_id`, чью ленту
 * надо перезапросить: портал пишет в чат задачи системное сообщение о смене
 * статуса.
 *
 * @param {number|string} taskId
 * @param {string} action
 * @returns {Promise<object>} `{action, ok, task, dialog_id}`
 */
export function taskAction(taskId, action) {
    const path = withSeg(withSeg(PATHS.taskAction, ':id', taskId), ':action', action);
    return call(path, jsonInit('POST', {}));
}

// ---------------------------------------------------------------------------
// Медиа (Ф8)
// ---------------------------------------------------------------------------

/**
 * Адрес прокси вложения.
 *
 * ЕДИНСТВЕННЫЙ адрес, по которому панель получает байты файла. Прямых ссылок
 * портала (`urlDownload`, `urlShow`, `unifiedLink`) в DTO нет вовсе — они и не
 * работают без сессионной куки, — а `downloadUrl` из `im.v2.File.download`
 * содержит код вебхука и до браузера не доезжает никогда.
 *
 * @param {string} dialogId — `chat27`, `sg12` или `19`
 * @param {number|string} messageId
 * @param {number} index — порядковый номер вложения в сообщении
 * @returns {string}
 */
export function mediaUrl(dialogId, messageId, index) {
    return withSeg(
        withSeg(withSeg(PATHS.mediaFile, ':dialog', dialogId), ':mid', messageId),
        ':index',
        Number.isFinite(Number(index)) ? Math.trunc(Number(index)) : 0,
    );
}

/** Адрес прокси аватара сотрудника (Р6). */
export function avatarUrl(userId) {
    return withSeg(PATHS.mediaAvatar, ':user_id', userId);
}

/**
 * Сырой `Response` вложения — для прогресса через `body.getReader()`.
 *
 * Возвращает именно `Response`, а не тело: разбирать поток обязан вызывающий,
 * иначе прогресс скачивания посчитать не из чего.
 *
 * Ошибочный статус превращается в `Error` с полем `.code` из конверта — тем же
 * разборщиком, что и у остальных роутов панели: решения принимаются по коду, а
 * не по HTTP-статусу и не по тексту.
 *
 * @param {string} path — результат [`mediaUrl`] либо [`avatarUrl`]
 * @returns {Promise<Response>}
 */
export async function fetchBytes(path) {
    const res = await apiFetch(path);
    if (res.ok) return res;
    // Тело ошибки — наш конверт `{"error":{code,message}}`; не-JSON (502 от
    // прокси) штатно превращается в «HTTP 502» без исключения.
    let body = null;
    try {
        body = await res.json();
    } catch (_) {
        body = null;
    }
    throw parseErrorEnvelope(res.status, body);
}

// ---------------------------------------------------------------------------
// Анализ чата через Claude (Ф12)
//
// Имена функций и формы тел БАЙТ В БАЙТ повторяют Telegram-ские
// (js/telegram/api.js) и VK-шные: попап один и тот же
// (js/messenger/analysis-core.js), и расхождение в транспорте означало бы
// «в Telegram работает, в Битрикс24 нет» — жалобу, которую невозможно
// диагностировать по симптому.
//
// Отличие от соседей ровно одно и лежит НЕ здесь: глубина выгрузки у портала
// своя (50/100/200 при потолке страницы в 50 сообщений и бюджете 2 rps), но
// это значение приходит аргументом из js/bitrix/analysis.js — транспорт про
// потолки не знает.
//
// Клиентского лимитера у панели нет (в отличие от VK с его 3 rps на токен):
// интенсивность душит сам портал, и его состояние `throttled` живёт отдельным
// путём (см. правило 2 в шапке файла). Поэтому вызовы идут прямо через общий
// `call`, разбирающий тот же конверт `{"error":{"code":…}}`.
// ---------------------------------------------------------------------------

/**
 * `GET /api/bitrix/dialogs/:id/prompt` — заметка пользователя к чату.
 *
 * `max_chars` приходит С СЕРВЕРА и не зашивается в JS: счётчик символов в
 * textarea обязан совпадать с лимитом, по которому сервер реально режет, —
 * иначе пользователь допишет фразу, которая молча пропадёт.
 *
 * Отсутствие заметки — это `{prompt: '', updated_at: null}`, а НЕ 404: «нет
 * заметки» и «пустая заметка» на этом роуте одно состояние.
 *
 * @param {string} dialogId — `chat27`, `sg12` или `19`
 * @returns {Promise<{prompt: string, updated_at: ?number, max_chars: number}>}
 */
export function getChatPrompt(dialogId) {
    return call(withSeg(PATHS.dialogPrompt, ':id', dialogId));
}

/**
 * `PUT /api/bitrix/dialogs/:id/prompt` — сохранить заметку.
 *
 * Пустая строка СТИРАЕТ заметку, и это единственный способ её стереть:
 * отдельного DELETE нет. Тело обязано присутствовать всегда — отсутствующий
 * или битый JSON сервер считает `400 invalid_input`, а не «стереть».
 *
 * @param {string} dialogId
 * @param {string} prompt
 * @returns {Promise<{prompt: string, updated_at: ?number, max_chars: number}>}
 */
export function putChatPrompt(dialogId, prompt) {
    return call(
        withSeg(PATHS.dialogPrompt, ':id', dialogId),
        jsonInit('PUT', { prompt: String(prompt == null ? '' : prompt) }),
    );
}

/**
 * `GET /api/bitrix/dialogs/:id/analysis` — последний СОХРАНЁННЫЙ разбор.
 *
 * Лежит в локальном файле, поэтому роут отвечает 200 и при отвязанном вебхуке:
 * попап показывает прошлый разбор даже когда выгрузить новую историю нечем.
 *
 * «Разбора нет» — это `{text: '', updated_at: null, last_message_id: null}`, а
 * НЕ 404: попап открывают и до первого запуска, и ошибка в этом месте была бы
 * враньём о нормальном состоянии.
 *
 * `thread` ВСЕГДА массив: сервер отдаёт `[]` там, где вопросов не задавали, а
 * потребитель (ядро попапа) обходит его циклом и не обязан помнить, что поля
 * может не быть вовсе (старый бэкенд, кэш PWA со старым фронтом).
 * Нормализация здесь, а не у потребителя: точка входа данных одна.
 *
 * @param {string} dialogId
 * @returns {Promise<{text: string, last_message_id: ?number,
 *   messages_used: number, updated_at: ?number, max_chars: number,
 *   thread: Array<{q: string, a: string, at: ?number}>}>}
 */
export async function getSavedAnalysis(dialogId) {
    const dto = await call(withSeg(PATHS.dialogAnalysis, ':id', dialogId));
    const raw = dto && Array.isArray(dto.thread) ? dto.thread : [];
    const thread = raw
        .filter((t) => t && typeof t === 'object')
        .map((t) => ({
            q: String(t.q == null ? '' : t.q),
            a: String(t.a == null ? '' : t.a),
            at: Number.isFinite(t.at) ? Number(t.at) : null,
        }));
    return Object.assign({}, dto, { thread });
}

/**
 * `DELETE /api/bitrix/dialogs/:id/analysis` — стереть сохранённый разбор.
 *
 * Заметку чата НЕ трогает: в попапе это две разные кнопки и в хранилище две
 * разные величины. `{cleared: false}` — не ошибка, а «стирать было нечего»
 * (две вкладки нажали подряд).
 *
 * @param {string} dialogId
 * @returns {Promise<{cleared: boolean}>}
 */
export function clearSavedAnalysis(dialogId) {
    return call(withSeg(PATHS.dialogAnalysis, ':id', dialogId), jsonInit('DELETE', {}));
}

/**
 * `DELETE /api/bitrix/dialogs/:id/analysis/thread` — стереть ТОЛЬКО тред.
 *
 * Отдельный путь, а не `?thread=1` у общего DELETE: так разведение кнопок
 * становится буквальным — CLEAR ✕ и CLEAR Q&A бьют в РАЗНЫЕ адреса и не могут
 * стереть чужое. Ни разбор, ни заметку чата не трогает. `{cleared: false}` —
 * не ошибка, а «стирать было нечего».
 *
 * @param {string} dialogId
 * @returns {Promise<{cleared: boolean}>}
 */
export function clearThread(dialogId) {
    return call(withSeg(PATHS.dialogAnalysisThread, ':id', dialogId), jsonInit('DELETE', {}));
}

/**
 * `POST /api/bitrix/dialogs/:id/analysis` — запустить разбор или задать вопрос.
 *
 * Ответ приходит СРАЗУ, в момент запуска процесса: текст модели едет кадрами
 * `analysis_delta` по /ws/bitrix, финал — `analysis_done`. Держать HTTP
 * открытым на всю генерацию нельзя, её оборвёт любой прокси между браузером и
 * devforge.
 *
 * `attached: true` означает, что нового процесса не запускалось — мы
 * подключились к уже идущему run'у (вторая вкладка, повторный клик).
 *
 * ТРИ вида запуска (`kind`):
 * - `analyze` — полный разбор выгруженной истории;
 * - `ask` — вопрос по той же истории (сохранённый разбор не трогает);
 * - `refresh` — дочитать только сообщения новее сохранённого разбора и
 *   вернуть ОБНОВЛЁННЫЙ ЦЕЛЬНЫЙ разбор, который заменит прошлый.
 *
 * ДВА исхода `refresh`, о которых обязан знать вызывающий:
 * - сохранённого разбора нет ⇒ `400 invalid_input` (сначала полный разбор);
 * - новых сообщений нет ⇒ `200 {run_id: null, no_new_messages: true}`. Это
 *   штатный ответ, а не ошибка: модель не запускалась, кадров не будет, и
 *   ждать их нельзя — попап навсегда остался бы в «идёт разбор».
 *
 * Необязательные поля НЕ отправляются вовсе, если их нет: сервер отличает
 * отсутствующий `depth` (взять значение по умолчанию) от переданного.
 *
 * @param {string} dialogId
 * @param {{kind: 'analyze'|'ask'|'refresh', question?: string, depth?: number,
 *   history?: Array<{q: string, a: string}>}} opts
 * @returns {Promise<{run_id: ?string, attached: boolean, messages_used: number,
 *   truncated: boolean, no_new_messages: boolean}>}
 */
export function startAnalysis(dialogId, opts) {
    const o = opts || {};
    const body = { kind: String(o.kind || 'analyze') };
    if (o.question != null && String(o.question) !== '') body.question = String(o.question);
    if (Number.isFinite(o.depth) && o.depth > 0) body.depth = Number(o.depth);
    if (Array.isArray(o.history) && o.history.length) {
        body.history = o.history.map((t) => ({
            q: String((t && t.q) || ''),
            a: String((t && t.a) || ''),
        }));
    }
    return call(withSeg(PATHS.dialogAnalysis, ':id', dialogId), jsonInit('POST', body));
}

/**
 * `POST /api/bitrix/dialogs/:id/analysis/cancel` — остановить идущий разбор.
 *
 * `runId` необязателен: без него сервер гасит тот run, что идёт по этому чату.
 * Именно так кнопка «стоп» продолжает работать после перезагрузки вкладки,
 * потерявшей `run_id`.
 *
 * `{cancelled: false}` — не ошибка: гасить было нечего (run уже закончился).
 *
 * @param {string} dialogId
 * @param {?string} [runId]
 * @returns {Promise<{cancelled: boolean}>}
 */
export function cancelAnalysis(dialogId, runId) {
    const body = {};
    if (runId) body.run_id = String(runId);
    return call(withSeg(PATHS.dialogAnalysisCancel, ':id', dialogId), jsonInit('POST', body));
}

// ---------------------------------------------------------------------------
// Голосовые сообщения (ГС-Ф3).
// ---------------------------------------------------------------------------

/** Подставляет все три сегмента адреса вложения. */
function voicePath(template, dialogId, messageId, index) {
    return withSeg(
        withSeg(withSeg(template, ':dialog', dialogId), ':mid', messageId),
        ':index',
        String(Number(index || 0)),
    );
}

/**
 * POST /api/bitrix/voice/:dialog/:mid/:index/transcribe — запустить расшифровку.
 *
 * Ответ приходит СРАЗУ: сама расшифровка идёт секунды или минуты, а прогресс
 * едет кадрами `voice_transcribe_*` по /ws/bitrix.
 *
 * ТРИ исхода: `{ready: true, text}` — уже посчитано (движок не запускался);
 * `{run_id, attached: false}` — прогон запущен; `{run_id, attached: true}` —
 * расшифровка этого вложения уже идёт (вторая вкладка, повторный клик).
 *
 * Решать по `err.code`, а не по тексту: `voice_not_audio`, `voice_too_long`,
 * `voice_no_ffmpeg`, `voice_no_whisper`, `voice_no_model`, `voice_no_key`,
 * `voice_busy`, `metal_unavailable`.
 *
 * @param {string} dialogId `chat123` | `sg45` | `77`
 * @param {number|string} messageId
 * @param {number} index номер вложения в сообщении
 * @param {{force?: boolean}} [opts] `force` — пересчитать, даже если готово
 * @returns {Promise<{run_id: ?string, attached: boolean, ready: boolean, engine: string, text?: string}>}
 */
export async function transcribeVoice(dialogId, messageId, index, opts) {
    const q = opts && opts.force ? '?force=1' : '';
    return call(
        voicePath(PATHS.voiceTranscribe, dialogId, messageId, index) + q,
        jsonInit('POST', {}),
    );
}

/**
 * GET /api/bitrix/voice/:dialog/:mid/:index/transcript — готовая расшифровка.
 *
 * Расшифровки нет — это `{ready: false}` с пустым текстом, а НЕ ошибка. Ни
 * одного обращения к порталу: ответ читается из локального файла, поэтому
 * работает и при исчерпанном лимите запросов.
 *
 * @param {string} dialogId
 * @param {number|string} messageId
 * @param {number} index
 * @returns {Promise<{ready: boolean, text: string, language: string, engine: string, duration_ms: number, created_at: ?number}>}
 */
export async function getVoiceTranscript(dialogId, messageId, index) {
    return call(voicePath(PATHS.voiceTranscript, dialogId, messageId, index));
}

/**
 * GET /api/bitrix/voice/settings — каким движком расшифровываются голосовые.
 *
 * Настройка ОБЩАЯ для всех мессенджеров: она живёт в `voice.toml` рядом с
 * ключом polza.ai, а не в `user_settings.json`. Поэтому три панели читают её
 * своими роутами, но видят одно и то же значение.
 *
 * Ключ облака НИКОГДА не приходит — только признак `cloud_key_set`.
 * `warning` (если он есть) объясняет, чего выбранному движку не хватает:
 * `voice_no_key`, `voice_no_model`, `voice_no_whisper`, `voice_no_ffmpeg`.
 *
 * Логина в мессенджере роут НЕ требует: движок настраивают до входа.
 *
 * @returns {Promise<{engine: string, cloud_key_set: boolean, warning?: {code: string, message: string}}>}
 */
export async function getVoiceSettings() {
    return call(PATHS.voiceSettings);
}

/**
 * PUT /api/bitrix/voice/settings — переключить движок расшифровки.
 *
 * Выбор СОХРАНЯЕТСЯ, даже если движок сейчас нерабочий: в ответе тогда придёт
 * `warning`. Отказ приходит только на чужое слово (`400 invalid_input`).
 *
 * @param {string} engine `local` | `cloud`
 * @returns {Promise<{engine: string, cloud_key_set: boolean, warning?: {code: string, message: string}}>}
 */
export async function setVoiceSettings(engine) {
    return call(PATHS.voiceSettings, jsonInit('PUT', { engine: String(engine || '') }));
}

