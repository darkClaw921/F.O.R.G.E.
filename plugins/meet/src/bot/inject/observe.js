// forge-meet: чтение состояния встречи по ПРОБАМ.
//
// Селекторы приезжают сюда ДАННЫМИ (`probes.toml` → JSON), а не зашиты в код:
// вёрстка площадок меняется чаще, чем мы выпускаем релизы. Этот файл описывает
// ТОЛЬКО как пробовать, но никогда — что именно.
//
// # Почему модуль отдельный, а не часть capture.js
//
// Потому что запись НЕ ЗАВИСИТ ни от одной пробы. `capture.js` живёт в
// инжекте, который ставится до навигации и работает всю встречу; наблюдение —
// это `Runtime.evaluate` раз в секунду, и его отказ обязан стоить имён
// спикеров, а не звука. Разные файлы делают эту границу видимой: ни одна
// функция отсюда не вызывается из тапов.
//
// # Ни одного исключения наружу
//
// Битый селектор из пользовательского TOML — рядовая ситуация («забыл кавычку
// в `[class*=`»). `querySelector` на нём бросает `SyntaxError`, и если дать
// исключению уйти, наблюдение умрёт целиком: вместе с именами пропадут и
// маркер конца встречи, и состояние микрофона. Поэтому КАЖДЫЙ вызов DOM
// обёрнут, а результатом промаха всегда является пустота.

/** Кандидат вида `text:…` — поиск по видимому тексту. */
export const FORGE_TEXT_PREFIX = "text:"; // FORGE_STRIP_FOR_WORKLET

/** Кандидат вида `attr:…` — имя атрибута, а не селектор. */
export const FORGE_ATTR_PREFIX = "attr:"; // FORGE_STRIP_FOR_WORKLET

/**
 * Кандидат вида `contains:…` — поиск текста среди ЛЮБЫХ элементов.
 *
 * Зачем отдельно от `text:`. `text:` ищет только среди КЛИКАБЕЛЬНЫХ элементов,
 * и это правильно для кнопок: попади в выборку предок, клик по нему
 * промахнулся бы. Но МАРКЕРЫ — лобби, конец встречи, отказ во входе — почти
 * всегда обычные `div`, и `text:` не находил их НИКОГДА. Промах при этом
 * выглядел ровно как «маркера на странице нет», то есть проба молча не
 * работала: ошибка того же рода, что ловит весь этот файл.
 *
 * Берётся САМЫЙ ГЛУБОКИЙ носитель текста: без этого условия подходили бы и
 * `body`, и каждый предок по цепочке, а видимость проверялась бы у контейнера
 * во весь экран.
 */
export const FORGE_CONTAINS_PREFIX = "contains:"; // FORGE_STRIP_FOR_WORKLET

/**
 * Потолок длины текста элемента-носителя маркера.
 *
 * Маркер — короткая фраза. Отсечка по длине выбрасывает предков (у `body`
 * текст всей страницы) ДО дорогих проверок и заодно бережёт время: обход всех
 * узлов идёт раз в секунду на живой встрече.
 */
export const FORGE_MARKER_TEXT_LIMIT = 400; // FORGE_STRIP_FOR_WORKLET

/** Сколько узлов максимум обходится при поиске `contains:`. */
export const FORGE_SCAN_LIMIT = 8000; // FORGE_STRIP_FOR_WORKLET

/**
 * Что считается кликабельным при поиске по тексту.
 *
 * Список узкий намеренно: поиск текста по ВСЕМ узлам находил бы предка
 * («body содержит слово „Присоединиться“»), а клик по нему промахнулся бы.
 */
export const FORGE_CLICKABLE = "button, a, [role=\"button\"], [role=\"link\"], input[type=\"button\"], input[type=\"submit\"]"; // FORGE_STRIP_FOR_WORKLET

/** Тексты, означающие «активного спикера нет». */
export const FORGE_EMPTY_TEXTS = ["", "—", "-", "–", "…", "..."]; // FORGE_STRIP_FOR_WORKLET

/** Видимый текст элемента: `value` у полей, `textContent` у всего прочего. */
export function forgeText(el) { // FORGE_STRIP_FOR_WORKLET
    if (!el) { return ""; }
    var raw = typeof el.value === "string" && el.value ? el.value : (el.textContent || "");
    return String(raw).replace(/\s+/g, " ").trim();
}

/**
 * Виден ли элемент И доступен ли он для клика.
 *
 * Проверка не косметическая, а несущая: площадки держат пре-join и панель
 * встречи В ОДНОМ ДОКУМЕНТЕ, пряча ненужный экран стилями. Без фильтра
 * «кнопка выхода найдена» становится истиной ещё до входа, и бот решает, что
 * он уже внутри, не нажав ни одной кнопки.
 *
 * Мерилом взят бокс, а не `hidden`/`display`: `[hidden]` перебивается любым
 * авторским `display` (ровно так устроена мок-встреча), а вычисленный стиль
 * пришлось бы спрашивать у `getComputedStyle` для каждого предка. Нулевой бокс
 * означает «не отрисован» при любой причине.
 *
 * Среда без layout (тесты на фейковом DOM) считается видимой: там проверяется
 * логика проб, а не отрисовка.
 */
export function forgeVisible(el) { // FORGE_STRIP_FOR_WORKLET
    if (!el) { return false; }
    // ВЫКЛЮЧЕННАЯ КНОПКА — НЕ КНОПКА. Площадка вправе оставить её в разметке
    // после нажатия (ровно так делает мок-встреча), и без этой проверки бот
    // видел бы «Продолжить в браузере» вечно и жал бы её по кругу, не
    // добираясь до входа. Найдено живым прогоном, а не рассуждением.
    if (el.disabled === true) { return false; }
    if (el.getAttribute && el.getAttribute("aria-disabled") === "true") { return false; }
    if (typeof el.getBoundingClientRect !== "function") { return true; }
    try {
        var r = el.getBoundingClientRect();
        return !!r && r.width > 0 && r.height > 0;
    } catch (e) {
        return true;
    }
}

/** Несёт ли ПРЯМОЙ потомок тот же текст — то есть носитель не самый глубокий. */
export function forgeChildCarriesText(el, needle) { // FORGE_STRIP_FOR_WORKLET
    var kids = (el && el.children) || [];
    for (var i = 0; i < kids.length; i++) {
        if (forgeText(kids[i]).toLowerCase().indexOf(needle) !== -1) { return true; }
    }
    return false;
}

/**
 * Видимые элементы, чей СОБСТВЕННЫЙ текст содержит подстроку.
 *
 * Порядок проверок выбран из-за цены: сначала длина текста, потом подстрока,
 * потом «нет ли того же текста у потомка», и только у выживших —
 * `getBoundingClientRect`, который заставляет браузер считать раскладку.
 * Обратный порядок стоил бы тысячи пересчётов раскладки в секунду.
 */
export function forgeContainsAll(root, needle) { // FORGE_STRIP_FOR_WORKLET
    if (!root || needle === "") { return []; }
    var all = [];
    try {
        all = root.querySelectorAll("*");
    } catch (e) { return []; }
    var limit = all.length < FORGE_SCAN_LIMIT ? all.length : FORGE_SCAN_LIMIT;
    var hits = [];
    for (var i = 0; i < limit; i++) {
        var el = all[i];
        var raw = el && el.textContent ? String(el.textContent) : "";
        if (raw === "" || raw.length > FORGE_MARKER_TEXT_LIMIT) { continue; }
        var text = forgeText(el).toLowerCase();
        if (text.indexOf(needle) === -1) { continue; }
        if (forgeChildCarriesText(el, needle)) { continue; }
        if (forgeVisible(el)) { hits.push(el); }
    }
    return hits;
}

/**
 * Все ВИДИМЫЕ элементы, подходящие ОДНОМУ кандидату.
 *
 * Исключений не бросает никогда: битый селектор — это пустой список, а не
 * упавшее наблюдение.
 */
export function forgeProbeAll(root, candidate) { // FORGE_STRIP_FOR_WORKLET
    if (!root || typeof candidate !== "string" || candidate === "") { return []; }
    if (candidate.indexOf(FORGE_ATTR_PREFIX) === 0) { return []; }
    if (candidate.indexOf(FORGE_CONTAINS_PREFIX) === 0) {
        return forgeContainsAll(root, candidate.slice(FORGE_CONTAINS_PREFIX.length).trim().toLowerCase());
    }
    if (candidate.indexOf(FORGE_TEXT_PREFIX) === 0) {
        var needle = candidate.slice(FORGE_TEXT_PREFIX.length).trim().toLowerCase();
        if (needle === "") { return []; }
        var clickable = [];
        try {
            clickable = Array.prototype.slice.call(root.querySelectorAll(FORGE_CLICKABLE));
        } catch (e) { return []; }
        var hits = [];
        for (var i = 0; i < clickable.length; i++) {
            if (forgeVisible(clickable[i]) && forgeText(clickable[i]).toLowerCase().indexOf(needle) !== -1) {
                hits.push(clickable[i]);
            }
        }
        return hits;
    }
    var found = [];
    try {
        found = Array.prototype.slice.call(root.querySelectorAll(candidate));
    } catch (e) {
        return [];
    }
    var visible = [];
    for (var k = 0; k < found.length; k++) {
        if (forgeVisible(found[k])) { visible.push(found[k]); }
    }
    return visible;
}

/**
 * Первое попадание по МАССИВУ кандидатов.
 *
 * Кандидаты перебираются СТРОГО ПО ПОРЯДКУ и перебор останавливается на первом
 * непустом: порядок в TOML — это приоритет, а не перечисление. Возвращается и
 * номер сработавшего кандидата: без него нельзя ни отладить пробу, ни доказать
 * тестом, что порядок соблюдён.
 */
export function forgeProbeHit(root, candidates) { // FORGE_STRIP_FOR_WORKLET
    if (!candidates || !candidates.length) { return null; }
    for (var i = 0; i < candidates.length; i++) {
        var found = forgeProbeAll(root, candidates[i]);
        if (found.length) {
            return { el: found[0], all: found, index: i, candidate: candidates[i] };
        }
    }
    return null;
}

/** Первый элемент по массиву кандидатов либо `null`. */
export function forgeProbeFirst(root, candidates) { // FORGE_STRIP_FOR_WORKLET
    var hit = forgeProbeHit(root, candidates);
    return hit ? hit.el : null;
}

/** Есть ли хоть один элемент по массиву кандидатов. */
export function forgeProbePresent(root, candidates) { // FORGE_STRIP_FOR_WORKLET
    return forgeProbeHit(root, candidates) !== null;
}

/**
 * Помечен ли элемент маркером — САМ или его потомок.
 *
 * Оба случая настоящие: у мок-встречи признак «говорит» висит на самой плитке
 * (`[data-speaking="true"]`), у Телемоста — на потомке (`[class*="rootStroke"]`
 * внутри плитки, измерено Ф0). Поддерживать надо оба, иначе один из наборов
 * проб молча давал бы «никто не говорит».
 */
export function forgeMarked(el, candidates) { // FORGE_STRIP_FOR_WORKLET
    if (!el || !candidates || !candidates.length) { return false; }
    for (var i = 0; i < candidates.length; i++) {
        var candidate = candidates[i];
        if (typeof candidate !== "string"
            || candidate.indexOf(FORGE_TEXT_PREFIX) === 0
            || candidate.indexOf(FORGE_CONTAINS_PREFIX) === 0) {
            continue;
        }
        try {
            if (el.matches && el.matches(candidate)) { return true; }
        } catch (e) { /* битый селектор — не повод падать */ }
        if (forgeProbeAll(el, candidate).length) { return true; }
    }
    return false;
}

/** Значение первого присутствующего атрибута из `attr:`-кандидатов. */
export function forgeKeyAttr(el, candidates) { // FORGE_STRIP_FOR_WORKLET
    if (!el || !el.getAttribute || !candidates) { return null; }
    for (var i = 0; i < candidates.length; i++) {
        var name = String(candidates[i] || "");
        if (name.indexOf(FORGE_ATTR_PREFIX) === 0) { name = name.slice(FORGE_ATTR_PREFIX.length); }
        if (name === "") { continue; }
        var value = null;
        try { value = el.getAttribute(name); } catch (e) { value = null; }
        if (value !== null && String(value).trim() !== "") { return String(value).trim(); }
    }
    return null;
}

/**
 * Участники по DOM-пробам.
 *
 * Имя берётся вложенной пробой, а при её промахе — текстом самой плитки:
 * протухший `participant_name` не имеет права обнулить список, если сами
 * плитки ещё находятся. Пустое имя законно — его подписывает уже Rust
 * («Участник N»), потому что нумерация обязана быть устойчивой между
 * наблюдениями, а DOM о своей нумерации ничего не знает.
 */
export function forgeReadParticipants(doc, probes) { // FORGE_STRIP_FOR_WORKLET
    var tiles = [];
    var hit = forgeProbeHit(doc, probes.participant || []);
    if (hit) { tiles = hit.all; }
    var out = [];
    for (var i = 0; i < tiles.length; i++) {
        var tile = tiles[i];
        var nameEl = forgeProbeFirst(tile, probes.participant_name || []);
        var name = nameEl ? forgeText(nameEl) : forgeText(tile);
        if (name.length > 80) { name = name.slice(0, 80); }
        out.push({
            key: forgeKeyAttr(tile, probes.participant_key_attr || []),
            name: name,
            speaking: forgeMarked(tile, probes.speaking_marker || [])
        });
    }
    return out;
}

/** Текст элемента «активный спикер» либо `null`, если говорящих нет. */
export function forgeReadActiveSpeaker(doc, probes, participants) { // FORGE_STRIP_FOR_WORKLET
    var el = forgeProbeFirst(doc, probes.active_speaker || []);
    if (el) {
        var text = forgeText(el);
        for (var i = 0; i < FORGE_EMPTY_TEXTS.length; i++) {
            if (text === FORGE_EMPTY_TEXTS[i]) { return null; }
        }
        return text === "" ? null : text;
    }
    // У Телемоста отдельного элемента нет вовсе: активный выводится из плиток
    // по индикатору обводки. Это не запасной путь, а основной для площадок
    // режима `Slots`.
    for (var k = 0; k < participants.length; k++) {
        if (participants[k].speaking && participants[k].name) { return participants[k].name; }
    }
    return null;
}

/** Ушла ли страница на пост-звонковый адрес. */
export function forgePostCallUrl(href, patterns) { // FORGE_STRIP_FOR_WORKLET
    if (!href || !patterns || !patterns.length) { return false; }
    for (var i = 0; i < patterns.length; i++) {
        var p = String(patterns[i] || "");
        if (p !== "" && String(href).indexOf(p) !== -1) { return true; }
    }
    return false;
}

/**
 * Полный снимок встречи глазами проб.
 *
 * `clock` приходит параметром, а не берётся из `performance` внутри: так
 * функция остаётся чистой и проверяемой в `node --test` на фейковом DOM. В
 * браузере её зовут с `{perf: performance.now(), unix: Date.now()}` —
 * ЕДИНСТВЕННЫМ измерением обоих часов в один момент, из которого Rust
 * однажды вычисляет начало оси и дальше пользуется только монотонным `perf`.
 */
export function forgeObserveMeeting(doc, probes, loc, clock) { // FORGE_STRIP_FOR_WORKLET
    var participants = forgeReadParticipants(doc, probes);
    var micOn = forgeProbePresent(doc, probes.mic_mute_button || []);
    var micOff = forgeProbePresent(doc, probes.mic_unmute_button || []);
    var camOn = forgeProbePresent(doc, probes.cam_disable_button || []);
    var camOff = forgeProbePresent(doc, probes.cam_enable_button || []);
    var href = loc && loc.href ? String(loc.href) : "";
    return {
        perf: clock && typeof clock.perf === "number" ? clock.perf : 0,
        unix: clock && typeof clock.unix === "number" ? clock.unix : 0,
        href: href,
        participants: participants,
        active: forgeReadActiveSpeaker(doc, probes, participants),
        speaking: participants.some ? participants.some(function (p) { return p.speaking; }) : false,
        // Состояние устройств: «включено» и «выключено» — РАЗНЫЕ пробы, и
        // отсутствие обеих означает «не знаю», а не «выключено». Ложное «уже
        // выключен» стоило бы бота, гудящего в чужую встречу.
        mic: micOn ? "on" : (micOff ? "off" : "unknown"),
        cam: camOn ? "on" : (camOff ? "off" : "unknown"),
        ui: {
            continueInBrowser: forgeProbePresent(doc, probes.continue_in_browser || []),
            nameInput: forgeProbePresent(doc, probes.name_input || []),
            joinButton: forgeProbePresent(doc, probes.join_button || []),
            leaveButton: forgeProbePresent(doc, probes.leave_button || []),
            lobby: forgeProbePresent(doc, probes.lobby_marker || []),
            captcha: forgeProbePresent(doc, probes.captcha_marker || []),
            // Кнопка «подключить звук компьютера». У всех, кроме Zoom, проба
            // пуста и шаг просто не существует.
            joinAudio: forgeProbePresent(doc, probes.join_audio_button || [])
        },
        // ОТКАЗ ВО ВХОДЕ — отдельный сигнал, а не разновидность «ничего не
        // нашли». Google Meet отклоняет анонимную заявку за секунды, и разница
        // между «нам отказали» и «мы ещё ждём» — это разница между честным
        // объяснением человеку и минутой молчания.
        denied: forgeProbePresent(doc, probes.join_denied_marker || []),
        ended: forgeProbePresent(doc, probes.meeting_ended_marker || []),
        postCall: forgePostCallUrl(href, probes.post_call_url || [])
    };
}
