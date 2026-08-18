// Тест клавиатурной навигации панели «Битрикс24» (js/bitrix/{keys,keynav}.js).
//
// ============================================================================
// ЧТО ЗДЕСЬ ЗАКРЕПЛЕНО И ПОЧЕМУ ИМЕННО ЭТО
// ============================================================================
//
// 1. СОПОСТАВЛЕНИЕ ПО `event.code`, А НЕ ПО `event.key`. Панель мессенджера —
//    место, где по-русски печатают чаще, чем по-английски. При русской
//    раскладке `event.key` физической клавиши J вернёт 'о', и хоткей `j`
//    перестал бы работать ровно у целевой аудитории. Тест шлёт события с
//    НЕЛАТИНСКИМ `key` и латинским `code` и требует, чтобы действие сработало.
//
// 2. `owns()` УСТУПАЕТ VIM ПО ФОКУСУ. `static/hotkeys.js` занимает j/k/h/l
//    глобально и спрашивает панель, забирать ли клавиши. Ответ «да» обязан
//    зависеть от МЕСТА ФОКУСА, а не только от открытой вкладки: кликнув в
//    сайдбар сессий, пользователь возвращает себе vim.
//
// 3. `preventDefault` ТОЛЬКО ПРИ ВЫПОЛНЕННОМ ДЕЙСТВИИ. Ложное «выполнено»
//    съедает клавишу у соседних обработчиков, ничего не сделав, — и это
//    невозможно заметить глазами: нажатие просто «иногда не работает».
//
// 4. ГЕЙТ ДЕФОЛТА `bitrix_hotkeys_enabled !== false`. Дефолт — ВКЛЮЧЕНО.
//    Подмена на `=== true` (идиома соседних настроек, где дефолт выключенный)
//    молча выключила бы навигацию всем, кто ни разу не открывал настройки.
//
// 5. ОТСУТСТВИЕ СЕКРЕТА В `_debugState()`. Снимок уезжает в e2e-логи и
//    артефакты Playwright. Вебхук Битрикс24 — это ВЕСЬ URL целиком, и попасть
//    туда фрагментом он не имеет права.
//
// Запуск: node tmux-web/tests/frontend/bitrix_keys.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import {
    $document, $input, $log, $login, $panel, $dialogList, $dirList, $taskCard, $webhook,
    dispatch, makeEl, setActive,
} from './bitrix_dom_stub.mjs';

// Настройки читаются лениво через публичный контракт — кладём его ДО импорта
// модулей панели, чтобы первый же вызов видел ту же живую ссылку, что и код.
const userSettings = {};
globalThis.window.ForgeApp = { state: { userSettings } };

const KEYS = await import(new URL('../../static/js/bitrix/keys.js', import.meta.url).href);
const NAV = await import(new URL('../../static/js/bitrix/keynav.js', import.meta.url).href);
const MAIN = await import(new URL('../../static/js/bitrix/main.js', import.meta.url).href);
// Карточка встречи: её гейт живёт в panelVisible(), и проверять его надо
// НАСТОЯЩИМ попапом — узел создаётся кодом, в разметке приложения его нет.
const MEET_POPUP = await import(
    new URL('../../static/js/messenger/meet-popup.js', import.meta.url).href
);

let failures = 0;
let checks = 0;

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    }
}

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    }
}

/** Нажатие клавиши: `code` — физическая клавиша, `key` — символ раскладки. */
function press(code, key, extra) {
    return dispatch($document, 'keydown', Object.assign({
        code,
        key: key === undefined ? '' : key,
        target: (extra && extra.target) || $panel,
        ctrlKey: false,
        altKey: false,
        shiftKey: false,
        metaKey: false,
    }, extra || {}));
}

/** Сбрасывает настройки к «ничего не задано» (дефолты). */
function resetSettings() {
    for (const k of Object.keys(userSettings)) delete userSettings[k];
    NAV.invalidateBindings();
}

/** Строка ленты с набором кнопок действий. */
function logRow(id, opts = {}) {
    const li = makeEl('li');
    li.className = 'b24-line' + (opts.system ? ' b24-line-system' : '');
    li.dataset.msgId = String(id);
    const text = makeEl('span');
    text.className = 'b24-line-text';
    text.textContent = opts.text || `сообщение ${id}`;
    li.appendChild(text);
    for (const action of (opts.actions || [])) {
        const btn = makeEl('button');
        btn.className = 'b24-line-btn';
        btn.dataset.action = action;
        li.appendChild(btn);
    }
    $log.appendChild(li);
    return li;
}

/** Строка колонки диалогов. */
function dialogRow(id, opts = {}) {
    const li = makeEl('li');
    li.className = 'b24-dialog' + (opts.active ? ' active' : '');
    li.dataset.dialogId = String(id);
    if (opts.unread) {
        const badge = makeEl('span');
        badge.className = 'b24-dialog-unread';
        badge.textContent = String(opts.unread);
        li.appendChild(badge);
    }
    $dialogList.appendChild(li);
    return li;
}

function clearRows() {
    $log.childNodes.slice().forEach((n) => n.remove());
    $dialogList.childNodes.slice().forEach((n) => n.remove());
    $dirList.childNodes.slice().forEach((n) => n.remove());
}

// ---------------------------------------------------------------------------
// 1. Реестр действий: форма и отсутствие конфликтов
// ---------------------------------------------------------------------------

{
    const ids = KEYS.B24_ACTIONS.map((a) => a.id);
    eq(new Set(ids).size, ids.length, 'реестр: id действий уникальны');
    const combos = KEYS.B24_ACTIONS.map((a) => String(a.combo).toLowerCase());
    eq(new Set(combos).size, combos.length,
        'реестр: дефолтные комбинации не конфликтуют между собой');
    const groups = new Set(KEYS.B24_ACTIONS.map((a) => a.group));
    for (const g of groups) {
        ok(KEYS.B24_ACTION_GROUPS.includes(g), `реестр: группа «${g}» объявлена в порядке групп`);
    }
    // Escape не переназначается: это выход из ЛЮБОГО режима, и переезд сломал
    // бы единственный способ отменить действие.
    const esc = KEYS.findAction('ui.escape');
    ok(esc && esc.fixed === true, 'реестр: ui.escape помечен fixed');
    // Действия, которых нет у соседних панелей, — ради них панель и своя.
    for (const id of ['msg.edit', 'msg.like', 'task.card', 'task.actions']) {
        ok(KEYS.findAction(id) !== null, `реестр: есть собственное действие ${id}`);
    }
    // Санитайзер переопределений отбрасывает неизвестные id: реестр знает
    // только фронт, и мусор из старого файла настроек не должен ничего ломать.
    const clean = KEYS.sanitizeOverrides({ 'nav.next': 'Ctrl+j', 'нет.такого': 'x' });
    eq(clean.get('nav.next'), 'Ctrl+j', 'sanitizeOverrides: известное действие сохранено');
    ok(!clean.has('нет.такого'), 'sanitizeOverrides: неизвестное действие отброшено');
}

// ---------------------------------------------------------------------------
// 2. Гейт дефолта: bitrix_hotkeys_enabled читается `!== false`
// ---------------------------------------------------------------------------

{
    resetSettings();
    ok(NAV.hotkeysEnabled(), 'настройка отсутствует — хоткеи ВКЛЮЧЕНЫ');
    userSettings.bitrix_hotkeys_enabled = true;
    ok(NAV.hotkeysEnabled(), 'явное true — включены');
    userSettings.bitrix_hotkeys_enabled = 'yes';
    ok(NAV.hotkeysEnabled(), 'не-false значение (строка) — включены');
    userSettings.bitrix_hotkeys_enabled = 0;
    ok(NAV.hotkeysEnabled(), '0 — не false, значит включены');
    userSettings.bitrix_hotkeys_enabled = false;
    ok(!NAV.hotkeysEnabled(), 'явное false — выключены');
    resetSettings();
    // Соседний флаг фильтра читается ИНАЧЕ (`=== true`) — и путать их нельзя.
    // Здесь это закреплено косвенно: выставленный фильтр не влияет на хоткеи.
    userSettings.bitrix_filter_enabled = false;
    ok(NAV.hotkeysEnabled(), 'выключенный фильтр не выключает хоткеи');
    resetSettings();
}

// ---------------------------------------------------------------------------
// 3. owns(): вкладка, оверлей и МЕСТО ФОКУСА
// ---------------------------------------------------------------------------

{
    resetSettings();
    setActive(null);
    $panel.hidden = false;
    $login.hidden = true;
    ok(NAV.b24OwnsKeyboard(), 'панель открыта, фокус нигде — клавиши её');

    setActive($input);
    ok(NAV.b24OwnsKeyboard(), 'фокус внутри панели — клавиши её');

    // Сайдбар сессий: узел ВНЕ #bitrix.
    const outside = makeEl('div');
    $document.body.appendChild(outside);
    setActive(outside);
    ok(!NAV.b24OwnsKeyboard(), 'фокус вне панели — vim забирает клавиши обратно');

    setActive(null);
    $panel.hidden = true;
    ok(!NAV.b24OwnsKeyboard(), 'вкладка закрыта — клавиши не её');
    $panel.hidden = false;

    $login.hidden = false;
    ok(!NAV.b24OwnsKeyboard(), 'открыт оверлей подключения — клавиши принадлежат форме');
    $login.hidden = true;

    // Карточка встречи. Без этой ветки vim-навигация забирает `j`/`k` у поля
    // поиска по расшифровке и у поля переименования участника. Проверяется
    // НАСТОЯЩИМ попапом: класс `.modal-overlay` он не несёт НАМЕРЕННО (тот
    // тянет глобальные токены и position: fixed), поэтому существующая
    // проверка модалки его не видит.
    {
        const popup = MEET_POPUP.createMeetPopup({
            panel: 'bitrix', host: $panel, doc: $document,
        });
        ok(NAV.panelVisible(), 'встреча: закрытая карточка навигации не мешает');
        popup.open({
            key: 'bitrix|chat123|7', panel: 'bitrix', chatId: 'chat123', messageId: 7, state: null,
        });
        ok(!NAV.panelVisible(), 'встреча: открытая карточка = панель не готова');
        ok(!NAV.b24OwnsKeyboard(), 'встреча: при открытой карточке владения нет');
        popup.close();
        ok(NAV.panelVisible(), 'встреча: закрытие карточки возвращает навигацию');
        popup.teardown();
    }

    userSettings.bitrix_hotkeys_enabled = false;
    ok(!NAV.b24OwnsKeyboard(), 'хоткеи выключены настройкой — клавиши не её');
    resetSettings();
}

// ---------------------------------------------------------------------------
// 4. Сопоставление по event.code: русская раскладка не ломает навигацию
// ---------------------------------------------------------------------------

{
    resetSettings();
    clearRows();
    NAV.initB24Keynav({ onNotice: () => {} });
    setActive(null);
    $panel.hidden = false;
    $login.hidden = true;

    dialogRow('chat7');
    dialogRow('19');
    NAV.runAction('focus.dialogs');

    // Физическая J при РУССКОЙ раскладке: key='о', code='KeyJ'.
    const e1 = press('KeyJ', 'о');
    eq(NAV._debugKeynavState().lastAction, 'nav.next',
        'русская раскладка: физическая J по-прежнему «следующий элемент»');
    eq(e1.defaultPrevented, true, 'выполненное действие гасит событие');

    // Обратная проверка: латинская 'j' в поле `key` при ЧУЖОМ коде действием
    // не считается — иначе сопоставление шло бы по key, а не по code.
    const before = NAV._debugKeynavState().lastAction;
    const e2 = press('Digit9', 'j');
    eq(NAV._debugKeynavState().lastAction, before,
        'key=j при чужом code действием не считается');
    eq(e2.defaultPrevented, false, 'несопоставленная клавиша не гасится');
}

// ---------------------------------------------------------------------------
// 5. preventDefault ТОЛЬКО при выполненном действии
// ---------------------------------------------------------------------------

{
    resetSettings();
    clearRows();
    setActive(null);

    // Лента пуста: копировать нечего, действие НЕ выполнено.
    const e1 = press('KeyY', 'н');
    eq(NAV._debugKeynavState().lastAction, 'msg.copy', 'msg.copy: диспетчер вызван');
    eq(e1.defaultPrevented, false,
        'msg.copy на пустой ленте: действие не выполнено — клавиша достаётся соседям');

    // Есть строка с текстом, но нет clipboard API — модуль сообщает об этом и
    // считает нажатие обработанным (уведомление показано).
    logRow(101, { text: 'привет' });
    const e2 = press('KeyY', 'н');
    eq(e2.defaultPrevented, true,
        'msg.copy без clipboard: уведомление показано — нажатие обработано');

    // Клавиша, за которой не стоит ни одного действия.
    const e3 = press('KeyZ', 'я');
    eq(e3.defaultPrevented, false, 'незанятая клавиша не гасится');

    // В поле ввода одиночные клавиши не перехватываются вовсе — иначе
    // сообщение с буквой «j» было бы не набрать.
    const e4 = press('KeyJ', 'о', { target: $input });
    eq(e4.defaultPrevented, false, 'в поле ввода одиночная клавиша принадлежит полю');

    // Модалка настроек поверх страницы: там наши клавиши обязаны молчать (в
    // том числе пока пользователь ЗАПИСЫВАЕТ новую комбинацию).
    const overlay = makeEl('div');
    overlay.className = 'modal-overlay';
    $document.body.appendChild(overlay);
    const e5 = press('KeyJ', 'о');
    eq(e5.defaultPrevented, false, 'при открытой модалке настроек панель молчит');
    overlay.remove();

    // Лайтбокс живёт по своим правилам (стрелки листают галерею, Esc
    // закрывает) — вмешиваться нельзя.
    const lightbox = $document.getElementById('b24-lightbox');
    lightbox.hidden = false;
    const e6 = press('KeyJ', 'о');
    eq(e6.defaultPrevented, false, 'при открытом лайтбоксе панель молчит');
    lightbox.hidden = true;

    // Выключенные настройкой хоткеи не перехватывают ничего.
    userSettings.bitrix_hotkeys_enabled = false;
    const e7 = press('KeyJ', 'о');
    eq(e7.defaultPrevented, false, 'выключенные хоткеи не гасят клавиши');
    resetSettings();
}

// ---------------------------------------------------------------------------
// 6. Переопределения из настроек ложатся ПОВЕРХ дефолтов
// ---------------------------------------------------------------------------

{
    resetSettings();
    clearRows();
    dialogRow('chat7');
    dialogRow('19');
    NAV.runAction('focus.dialogs');

    userSettings.bitrix_hotkeys = { 'nav.next': 'Ctrl+j' };
    // Кэш привязок ключуется СОДЕРЖИМЫМ карты, поэтому новая настройка
    // действует сразу — без перезагрузки страницы и без явного invalidate.
    const eCtrl = press('KeyJ', 'о', { ctrlKey: true });
    eq(NAV._debugKeynavState().lastAction, 'nav.next',
        'переопределение: Ctrl+J стал «следующим элементом»');
    eq(eCtrl.defaultPrevented, true, 'переопределение: действие выполнено');

    // Прежняя клавиша освободилась целиком: одиночная J больше ничего не
    // делает (иначе действие оказалось бы на ДВУХ клавишах разом).
    const plain = press('KeyJ', 'о');
    eq(plain.defaultPrevented, false, 'переопределение: прежняя клавиша освободилась');

    // Мусор в карте не ломает разбор и не отменяет остальные привязки.
    userSettings.bitrix_hotkeys = { 'nav.next': '', 'нет.такого': 'x' };
    const back = press('KeyJ', 'о');
    eq(back.defaultPrevented, true, 'мусорное переопределение: действует дефолт');

    // Явный сброс кэша (контракт для формы настроек) ничего не ломает.
    NAV.invalidateBindings();
    const after = press('KeyJ', 'о');
    eq(after.defaultPrevented, true, 'после invalidateBindings привязки те же');
    resetSettings();
}

// ---------------------------------------------------------------------------
// 7. Действия над строкой идут ЧЕРЕЗ КНОПКИ строки
// ---------------------------------------------------------------------------

{
    resetSettings();
    clearRows();
    const notices = [];
    NAV.initB24Keynav({ onNotice: (t) => notices.push(t) });

    // Своё сообщение: есть и лайк, и ответ, и правка.
    const own = logRow(201, { actions: ['like', 'reply', 'edit'] });
    NAV.runAction('focus.log');

    press('KeyR', 'к');
    eq(own.querySelector('.b24-line-btn').parentNode === own, true, 'строка на месте');
    eq(own.querySelectorAll('.b24-line-btn').find((b) => b.dataset.action === 'reply').clickCount, 1,
        'msg.reply: нажата кнопка «Ответить» строки под курсором');
    press('KeyE', 'у');
    eq(own.querySelectorAll('.b24-line-btn').find((b) => b.dataset.action === 'edit').clickCount, 1,
        'msg.edit: нажата кнопка «Изменить»');
    press('KeyL', 'д');
    eq(own.querySelectorAll('.b24-line-btn').find((b) => b.dataset.action === 'like').clickCount, 1,
        'msg.like: нажата кнопка лайка');

    // Чужое сообщение: правки у него нет — портал её не разрешает.
    clearRows();
    const alien = logRow(202, { actions: ['like', 'reply'] });
    NAV.runAction('focus.log');
    notices.length = 0;
    const eEdit = press('KeyE', 'у');
    eq(alien.querySelectorAll('.b24-line-btn').length, 2, 'у чужой строки кнопки правки нет');
    eq(notices.length, 1, 'msg.edit на чужом сообщении: показано объяснение');
    eq(eEdit.defaultPrevented, true,
        'объяснение показано — нажатие обработано, иначе vim увёл бы фокус и спрятал его');

    // Системные строки курсору не нужны: на них нечего ответить.
    clearRows();
    logRow(203, { system: true });
    const real = logRow(204, { actions: ['reply'] });
    NAV.runAction('focus.log');
    press('KeyR', 'к');
    eq(real.querySelectorAll('.b24-line-btn')[0].clickCount, 1,
        'курсор встал на обычную строку, а не на системную');
}

// ---------------------------------------------------------------------------
// 8. Карточка задачи — собственная зона панели
// ---------------------------------------------------------------------------

{
    resetSettings();
    clearRows();
    const notices = [];
    NAV.initB24Keynav({ onNotice: (t) => notices.push(t) });

    // Обычный чат: карточки нет вовсе.
    $taskCard.hidden = true;
    notices.length = 0;
    const eNoCard = press('KeyC', 'с');
    eq(notices.length, 1, 'task.card в обычном чате: сказано, что это не чат задачи');
    eq(eNoCard.defaultPrevented, true, 'объяснение показано — нажатие обработано');

    // Чат задачи: карточка на месте, но действий портал не разрешает.
    $taskCard.hidden = false;
    notices.length = 0;
    press('KeyC', 'с');
    eq($taskCard.focusCount > 0, true, 'task.card: фокус переехал на карточку');
    eq(notices.length, 0, 'task.card: объяснять нечего — карточка есть');

    notices.length = 0;
    press('KeyX', 'ч');
    eq(notices.length, 1, 'task.actions без кнопок: сказано, что портал их не разрешает');

    // Появилось разрешённое действие.
    const btn = makeEl('button');
    btn.className = 'b24-task-btn';
    btn.dataset.b24Task = 'complete';
    $taskCard.appendChild(btn);
    notices.length = 0;
    press('KeyX', 'ч');
    eq(btn.focusCount, 1, 'task.actions: фокус встал на первую разрешённую кнопку');
    eq(notices.length, 0, 'task.actions: объяснять нечего');

    // Enter на уже сфокусированной кнопке НЕ нажимается нами: браузер сделает
    // это сам, а второй click отправил бы порталу два действия на одно нажатие.
    setActive(btn);
    const eEnter = press('Enter', 'Enter');
    eq(btn.clickCount, 0, 'nav.activate на кнопке: клик не дублируется');
    eq(eEnter.defaultPrevented, false, 'nav.activate на кнопке: событие не гасится');

    btn.remove();
    $taskCard.hidden = true;
    setActive(null);
}

// ---------------------------------------------------------------------------
// 9. Непрочитанные и директории
// ---------------------------------------------------------------------------

{
    resetSettings();
    clearRows();
    const notices = [];
    NAV.initB24Keynav({ onNotice: (t) => notices.push(t) });

    dialogRow('chat1');
    const unread = dialogRow('chat2', { unread: 3 });
    dialogRow('chat3');
    NAV.runAction('focus.dialogs');
    press('KeyU', 'г');
    eq(unread.className.includes('b24-kb-cursor'), true,
        'dialog.unread: курсор встал на непрочитанный диалог');

    clearRows();
    dialogRow('chat1');
    notices.length = 0;
    NAV.runAction('focus.dialogs');
    press('KeyU', 'г');
    eq(notices.length, 1, 'dialog.unread без непрочитанных: сказано словами');

    // Директории ЛОКАЛЬНЫЕ: переключение — клик по пункту, без запроса.
    const d1 = makeEl('li');
    d1.className = 'b24-dir active';
    d1.dataset.dir = 'all';
    const d2 = makeEl('li');
    d2.className = 'b24-dir';
    d2.dataset.dir = 'chats';
    $dirList.appendChild(d1);
    $dirList.appendChild(d2);
    const eDir = press('BracketRight', ']');
    eq(d2.clickCount, 1, 'dir.next: следующая директория выбрана кликом');
    eq(eDir.defaultPrevented, true, 'dir.next: действие выполнено');
}

// ---------------------------------------------------------------------------
// 10. Шпаргалка строится заново и закрывается по Escape
// ---------------------------------------------------------------------------

{
    resetSettings();
    setActive(null);
    ok(NAV.showHelp(), 'шпаргалка построена');
    ok(NAV._debugKeynavState().helpVisible, 'шпаргалка видна');
    const help = $panel.querySelector('.b24-keys-help');
    ok(help !== null, 'узел шпаргалки лежит внутри #bitrix');
    // По строке на действие — иначе половина клавиш осталась бы неизвестной.
    eq(help.querySelectorAll('.b24-keys-help-row').length, KEYS.B24_ACTIONS.length,
        'шпаргалка: по строке на каждое действие');
    const eEsc = press('Escape', 'Escape');
    eq(eEsc.defaultPrevented, true, 'Escape закрыл шпаргалку и погасил событие');
    ok(!NAV._debugKeynavState().helpVisible, 'шпаргалка скрыта');
}

// ---------------------------------------------------------------------------
// 11. teardown снимает слушатель
// ---------------------------------------------------------------------------

{
    resetSettings();
    clearRows();
    dialogRow('chat7');
    NAV.teardownB24Keynav();
    const e = press('KeyJ', 'о');
    eq(e.defaultPrevented, false, 'после teardown клавиши панели не перехватываются');
    // И возвращается обратно — панель переживает повторный вход на вкладку.
    NAV.initB24Keynav({ onNotice: () => {} });
    NAV.runAction('focus.dialogs');
    const e2 = press('KeyJ', 'о');
    eq(e2.defaultPrevented, true, 'после повторного init клавиши снова работают');
}

// ---------------------------------------------------------------------------
// 12. Контракт для hotkeys.js и РЕЕСТР владельцев клавиатуры
// ---------------------------------------------------------------------------

{
    const app = globalThis.window.ForgeApp;
    ok(app.b24Keys && typeof app.b24Keys.owns === 'function', 'опубликован контракт b24Keys.owns');
    ok(typeof app.b24Keys.enabled === 'function', 'опубликован b24Keys.enabled');
    ok(typeof app.b24Keys.invalidate === 'function', 'опубликован b24Keys.invalidate');
    // Реестр: панель регистрирует СЕБЯ САМА — до неё имена были захардкожены
    // в static/hotkeys.js, и третья панель была тем самым поводом ввести реестр.
    ok(Array.isArray(app.keyboardOwners), 'реестр владельцев клавиатуры создан');
    ok(app.keyboardOwners.includes('b24Keys'), 'панель зарегистрировала себя в реестре');
    // Повторный init не удваивает запись.
    NAV.initB24Keynav();
    eq(app.keyboardOwners.filter((n) => n === 'b24Keys').length, 1,
        'повторная инициализация не дублирует имя в реестре');
}

// ---------------------------------------------------------------------------
// 13. `_debugState()` — полнота и ОТСУТСТВИЕ СЕКРЕТА
// ---------------------------------------------------------------------------

{
    // Пользователь вставил вебхук в поле формы подключения — это единственное
    // место во фронте, где секрет вообще появляется.
    $webhook.value = 'https://portal.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/';

    const snap = MAIN._debugState();
    // Полнота: агрегат обязан покрывать ВСЕ модули панели, иначе e2e полезет
    // разбирать вёрстку и станет хрупким к любой косметике.
    for (const key of [
        'ws', 'login', 'dialogs', 'chat', 'media', 'upload', 'tasks',
        'mobile', 'theme', 'keys', 'filter',
    ]) {
        ok(Object.prototype.hasOwnProperty.call(snap, key),
            `_debugState содержит раздел ${key}`);
    }
    ok(typeof snap.keys.owns === 'boolean', '_debugState.keys.owns — булево (для e2e)');
    ok(typeof snap.mobile.narrow === 'boolean', '_debugState.mobile.narrow — булево');
    ok(typeof snap.filter.active === 'boolean', '_debugState.filter.active — булево');

    const serialized = JSON.stringify(snap);
    ok(!serialized.includes('wqdzbfhmn83d23j4'),
        'СЕКРЕТ: кода вебхука в снимке нет');
    ok(!serialized.includes('/rest/'),
        'СЕКРЕТ: фрагмента адреса вебхука в снимке нет');
    ok(!serialized.includes('bitrix24.ru'),
        'СЕКРЕТ: адреса портала в снимке нет');
    // Факт «поле заполнено» при этом виден — им e2e отличает набранный вебхук
    // от пустой формы, не зная ни одного его символа.
    eq(snap.login.webhookInDom, true, 'снимок показывает ФАКТ заполнения поля');

    $webhook.value = '';
}

console.log(`${checks - failures}/${checks} ассертов прошло (frontend/bitrix_keys)`);
if (failures > 0) process.exit(1);
