// tmux-web — общий слой мессенджеров: разбор и БЕЗОПАСНЫЙ рендер Markdown.
//
// Зачем модуль вообще существует. Ответы Claude (разбор чата и тред вопросов
// в панели VK) приходят с Markdown-разметкой и до сих пор показывались сырым
// текстом: пользователь читал `**жирный**` и `|таблица|` буквально.
//
// Почему модуль лежит в `js/messenger/`, а не в `js/vk/`. Разбор Markdown —
// не свойство ВКонтакте: те же ответы модели будут показывать панели Telegram
// и Битрикс24. Общий слой мессенджеров (`follow-core.js`, `hotkeys-core.js`,
// `http-envelope.js`, `theme-core.js`, `ws-core.js`) — уже сложившееся место
// для такого кода, и модуль написан по его правилам: никаких импортов панелей,
// никакого обращения к глобальному `document`.
//
// ═══ ГЛАВНЫЙ ИНВАРИАНТ ФАЙЛА ═══
// Ни одного `innerHTML` / `insertAdjacentHTML` / `outerHTML` / `document.write`
// — НИ В ОДНОЙ строке. Это не стилистика: текст, который сюда попадает,
// порождён моделью ИЗ ПЕРЕПИСКИ С ПОСТОРОННИМИ ЛЮДЬМИ, то есть это
// недоверенный ввод ровно в том же смысле, что и тело сообщения. Инвариант
// панели VK «в DOM только через textContent» (js/vk/main.js:44-45)
// распространяется и на этот модуль. Структурно инвариант держит секция 4
// теста tmux-web/tests/frontend/messenger_markdown.test.mjs: она читает ЭТОТ
// ФАЙЛ как текст и падает, если запрещённая строка вернётся.
//
// Готовая библиотека (marked/markdown-it/DOMPurify) не подключается: devforge
// обязан работать офлайн и не тянуть внешние ассеты, вендорить парсер с его
// собственной поверхностью XSS ради шести видов разметки — плохой размен, а
// весь риск здесь сосредоточен в ОДНОЙ функции `safeHref`, которую проще
// доказать, чем аудировать чужой санитайзер.
//
// ═══ АРХИТЕКТУРА: парсер отдельно от рендерера ═══
//   parseMarkdown(text)      → Array<Block>   — ЧИСТАЯ функция, DOM не трогает
//   renderMarkdown(ast, doc) → DocumentFragment — узлы только createElement
//   renderMarkdownText(text, doc)              — обёртка parse + render
//
// Разделение не эстетическое: чистый парсер тестируется без фейкового DOM,
// весь объём разметки покрывается сравнением структур, а рендерер получает
// `doc` ПАРАМЕТРОМ (а не берёт глобальный `document`) — и потому проверяется
// на фейковом документе в Node.
//
// ═══ ОБЪЁМ ПОДДЕРЖКИ (сознательно неполный CommonMark) ═══
// Блоки:   заголовки `#`…`######`, маркированные и нумерованные списки с
//          вложенностью, ограждённые блоки кода ``` / ~~~ (язык сохраняется,
//          ПОДСВЕТКИ НЕТ), цитаты `>`, горизонтальная линия, GFM-таблицы,
//          абзацы.
// Инлайн:  `**жирный**`, `*курсив*` / `_курсив_`, `` `код` ``,
//          `~~зачёркнутый~~`, ссылки `[текст](url)`.
// Чего НЕТ намеренно: изображения (внешний запрос из панели с перепиской —
// это утечка Referer'а и трекинг-пиксель), HTML-вставки (см. инвариант),
// сноски, списки определений, автоссылки без скобок, заголовки setext.
//
// ПРАВИЛО ДЕГРАДАЦИИ: непонятная или битая разметка становится обычным
// текстом, а НЕ пропадает. Терять кусок ответа модели нельзя — пользователь
// не узнает, что чего-то не увидел.
//
// ═══ ФОРМА AST ═══
// @typedef {{type:'paragraph', text:string, inline:Inline[]}} ParagraphBlock
// @typedef {{type:'heading', level:number, text:string, inline:Inline[]}} HeadingBlock
// @typedef {{type:'hr'}} HrBlock
// @typedef {{type:'code', lang:?string, text:string}} CodeBlock
// @typedef {{type:'blockquote', blocks:Block[]}} QuoteBlock
// @typedef {{text:string, inline:Inline[], children:Block[]}} ListItem
// @typedef {{type:'list', ordered:boolean, start:?number, marker:string, items:ListItem[]}} ListBlock
// @typedef {{text:string, inline:Inline[]}} TableCell
// @typedef {{type:'table', align:Array<?string>, header:TableCell[], rows:TableCell[][]}} TableBlock
// @typedef {ParagraphBlock|HeadingBlock|HrBlock|CodeBlock|QuoteBlock|ListBlock|TableBlock} Block
//
// @typedef {{type:'text', value:string}} TextInline
// @typedef {{type:'code', value:string}} CodeInline
// @typedef {{type:'strong'|'em'|'strike', children:Inline[]}} MarkInline
// @typedef {{type:'link', href:string, children:Inline[]}} LinkInline
// @typedef {TextInline|CodeInline|MarkInline|LinkInline} Inline
//
// У блоков с инлайн-содержимым СОХРАНЯЕТСЯ и исходный `text`: по нему тесты
// проверяют границы блоков, не увязая в дереве инлайна, а вызывающий код
// может показать сырой текст (так делает стриминг разбора — до кадра
// `analysis_done` панель рисует сырьё, см. ТЗ, раздел «Стриминг и Markdown»).

// Ширина табуляции при подсчёте отступа. 4 — то, что подставляет и Claude, и
// большинство редакторов; от этого числа зависит только уровень вложенности
// списков, поэтому точность здесь не критична.
const TAB_WIDTH = 4;

// Максимальная глубина вложенности списков и цитат. Ограничение стоит не от
// «кривых» входов, а от злонамеренных: тысяча уровней `>` — это рекурсия на
// тысячу кадров в рендерере. Всё, что глубже, остаётся текстом.
const MAX_NESTING = 12;

/** Символы, которые можно экранировать обратным слэшем. */
const ESCAPABLE = '\\`*_~[]()#+-.!>|';

// ==========================================================================
// Блочный парсер
// ==========================================================================

/**
 * Разбирает Markdown-текст в массив блоков.
 *
 * ЧИСТАЯ функция: не читает `document`, `window`, время или сеть — один и тот
 * же вход всегда даёт одинаковый выход. Именно поэтому её можно целиком
 * покрыть тестами в Node без фейкового DOM.
 *
 * @param {string} text — сырой текст ответа модели
 * @returns {Block[]} — пустой массив на пустом/непригодном входе
 */
export function parseMarkdown(text) {
    if (typeof text !== 'string' || text === '') return [];
    // Нормализация переводов строк ДО всего остального: файл из Windows и
    // ответ по WebSocket приходят с разными окончаниями, а весь парсер
    // построчный.
    const lines = text.replace(/\r\n?/g, '\n').split('\n');
    return parseBlocks(lines, 0);
}

/**
 * @param {string[]} lines
 * @param {number} depth — текущая глубина вложенности (цитаты/списки)
 * @returns {Block[]}
 */
function parseBlocks(lines, depth) {
    const out = [];
    let i = 0;
    while (i < lines.length) {
        const line = lines[i];

        if (isBlank(line)) { i += 1; continue; }

        const fence = matchFence(line);
        if (fence) {
            const res = readFence(lines, i, fence);
            out.push(res.node);
            i = res.next;
            continue;
        }

        if (isHr(line)) { out.push({ type: 'hr' }); i += 1; continue; }

        const heading = matchHeading(line);
        if (heading) {
            out.push({
                type: 'heading',
                level: heading.level,
                text: heading.text,
                inline: parseInline(heading.text),
            });
            i += 1;
            continue;
        }

        if (depth < MAX_NESTING && isQuote(line)) {
            const res = readQuote(lines, i, depth);
            out.push(res.node);
            i = res.next;
            continue;
        }

        if (isTableStart(lines, i)) {
            const res = readTable(lines, i);
            out.push(res.node);
            i = res.next;
            continue;
        }

        if (depth < MAX_NESTING && matchItem(line)) {
            const res = readList(lines, i, depth);
            out.push(res.node);
            i = res.next;
            continue;
        }

        // Абзац: всё, что не подошло ни под одно правило. Копится до пустой
        // строки или до начала другого блока — список сразу после текста БЕЗ
        // пустой строки обязан разбираться как список, иначе разметка модели
        // (она пустых строк перед списками часто не ставит) не сработает.
        const buf = [];
        while (i < lines.length) {
            const ln = lines[i];
            if (isBlank(ln)) break;
            if (buf.length > 0 && startsNewBlock(lines, i, depth)) break;
            buf.push(ln.trim());
            i += 1;
        }
        const paraText = buf.join('\n');
        out.push({ type: 'paragraph', text: paraText, inline: parseInline(paraText) });
    }
    return out;
}

/** Начинается ли на строке i блок, обрывающий абзац. */
function startsNewBlock(lines, i, depth) {
    const line = lines[i];
    if (matchFence(line)) return true;
    if (isHr(line)) return true;
    if (matchHeading(line)) return true;
    if (depth < MAX_NESTING && isQuote(line)) return true;
    if (depth < MAX_NESTING && matchItem(line)) return true;
    if (isTableStart(lines, i)) return true;
    return false;
}

function isBlank(line) {
    return /^[ \t]*$/.test(line);
}

/** Отступ строки в пробелах (табуляция разворачивается до TAB_WIDTH). */
function indentWidth(line) {
    let w = 0;
    for (let i = 0; i < line.length; i += 1) {
        const ch = line[i];
        if (ch === ' ') w += 1;
        else if (ch === '\t') w += TAB_WIDTH - (w % TAB_WIDTH);
        else break;
    }
    return w;
}

/**
 * Горизонтальная линия: три и более `-`, `*` или `_` одного вида, между ними
 * допустимы пробелы. Проверяется РАНЬШЕ списка, иначе `---` стал бы пунктом.
 */
function isHr(line) {
    return /^ {0,3}([-*_])[ \t]*(?:\1[ \t]*){2,}$/.test(line);
}

/**
 * Заголовок ATX. Семь решёток заголовком НЕ являются (уровня 7 в HTML нет) —
 * `####### seven` уходит в абзац целиком, вместе с решётками.
 */
function matchHeading(line) {
    const m = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?[ \t]*$/.exec(line);
    if (!m) return null;
    return { level: m[1].length, text: (m[2] || '').replace(/[ \t]+#+[ \t]*$/, '') };
}

/** Открывающее ограждение блока кода: ``` или ~~~ (оба поддержаны). */
function matchFence(line) {
    const m = /^([ \t]*)(`{3,}|~{3,})[ \t]*(.*)$/.exec(line);
    if (!m) return null;
    // ``` внутри строки с языком не может содержать обратных кавычек — иначе
    // это инлайн-код, а не ограждение.
    if (m[2][0] === '`' && m[3].includes('`')) return null;
    return { indent: m[1], fence: m[2], info: m[3].trim() };
}

/**
 * Читает ограждённый блок кода.
 *
 * Незакрытое ограждение считается закрытым в конце текста: модель регулярно
 * обрывает вывод на середине блока, и хвост ответа терять нельзя.
 * Содержимое НЕ проходит инлайн-разбор — звёздочки и `[текст](url)` внутри
 * кода обязаны остаться символами.
 */
function readFence(lines, start, fence) {
    const body = [];
    let i = start + 1;
    const closeRe = new RegExp(`^[ \\t]*${fence.fence[0] === '`' ? '`' : '~'}{${fence.fence.length},}[ \\t]*$`);
    while (i < lines.length && !closeRe.test(lines[i])) {
        body.push(lines[i]);
        i += 1;
    }
    const closed = i < lines.length;
    // Первое слово строки открытия — язык. Подсветки нет (решение ТЗ), язык
    // нужен только как data-атрибут: по нему можно будет подсветить позже, не
    // трогая парсер.
    const lang = fence.info ? fence.info.split(/\s+/)[0] : '';
    return {
        node: { type: 'code', lang: lang || null, text: body.join('\n') },
        next: closed ? i + 1 : i,
    };
}

function isQuote(line) {
    return /^ {0,3}>/.test(line);
}

/**
 * Цитата: подряд идущие строки с `>`. Маркер снимается один раз, остаток
 * рекурсивно разбирается как блоки — поэтому `> > текст` даёт вложенную
 * цитату, а `> # заголовок` — заголовок внутри цитаты.
 */
function readQuote(lines, start, depth) {
    const inner = [];
    let i = start;
    while (i < lines.length && isQuote(lines[i])) {
        inner.push(lines[i].replace(/^ {0,3}> ?/, ''));
        i += 1;
    }
    return { node: { type: 'blockquote', blocks: parseBlocks(inner, depth + 1) }, next: i };
}

/**
 * Пункт списка. Текст может отсутствовать (`-` сам по себе) — такой пункт
 * пустой, но парсер об этом не спотыкается.
 */
function matchItem(line) {
    const m = /^([ \t]*)(?:([-*+])|(\d{1,9})([.)]))(?:[ \t]+(.*))?$/.exec(line);
    if (!m) return null;
    const ordered = m[3] !== undefined;
    return {
        indent: indentWidth(line),
        ordered,
        marker: ordered ? m[4] : m[2],
        number: ordered ? Number(m[3]) : null,
        text: m[5] === undefined ? '' : m[5],
    };
}

/** Тот же ли это список: тип и вид маркера должны совпадать. */
function sameKind(a, b) {
    return a.ordered === b.ordered && a.marker === b.marker;
}

/**
 * Читает список, начиная со строки `start`.
 *
 * Вложенность — по отступу: любой пункт с отступом БОЛЬШЕ базового открывает
 * вложенный список внутри последнего пункта. Смена маркера (`-` → `*`) или
 * типа (`-` → `1.`) на том же уровне начинает НОВЫЙ список: так пишут, когда
 * хотят два списка подряд, и склеивать их нельзя.
 */
function readList(lines, start, depth) {
    const first = matchItem(lines[start]);
    const baseIndent = first.indent;
    const items = [];
    let i = start;

    while (i < lines.length) {
        if (isBlank(lines[i])) {
            // Пустая строка внутри списка допустима, если дальше идёт
            // продолжение того же списка; иначе список закончился.
            const j = nextNonBlank(lines, i);
            if (j >= lines.length) break;
            const m = matchItem(lines[j]);
            const continues = (m && m.indent >= baseIndent && sameKind(m, first))
                || indentWidth(lines[j]) > baseIndent;
            if (!continues) break;
            i = j;
            continue;
        }

        const m = matchItem(lines[i]);
        if (!m) break;
        if (m.indent < baseIndent) break;
        if (m.indent === baseIndent && !sameKind(m, first)) break;
        if (m.indent > baseIndent) {
            // Вложенный список без предшествующего пункта на этом уровне —
            // редкий, но реальный вход. Разбираем как вложенный в последний
            // пункт, а если пунктов ещё нет — как отдельный список.
            const res = readList(lines, i, depth + 1);
            if (items.length > 0) items[items.length - 1].children.push(res.node);
            else items.push({ text: '', inline: [], children: [res.node] });
            i = res.next;
            continue;
        }

        // Собственно пункт.
        const buf = [m.text];
        const children = [];
        i += 1;
        while (i < lines.length) {
            const ln = lines[i];
            if (isBlank(ln)) break;
            const mm = matchItem(ln);
            if (mm && mm.indent > baseIndent) {
                if (depth + 1 >= MAX_NESTING) { buf.push(ln.trim()); i += 1; continue; }
                const res = readList(lines, i, depth + 1);
                children.push(res.node);
                i = res.next;
                continue;
            }
            if (mm) break;
            if (indentWidth(ln) > baseIndent) {
                // Продолжение пункта: строка с отступом без маркера.
                buf.push(ln.trim());
                i += 1;
                continue;
            }
            break;
        }
        const text = buf.join('\n');
        items.push({ text, inline: parseInline(text), children });
    }

    return {
        node: {
            type: 'list',
            ordered: first.ordered,
            start: first.ordered ? first.number : null,
            marker: first.marker,
            items,
        },
        next: i,
    };
}

function nextNonBlank(lines, i) {
    let j = i;
    while (j < lines.length && isBlank(lines[j])) j += 1;
    return j;
}

// ==========================================================================
// GFM-таблицы
// ==========================================================================

/**
 * Таблица опознаётся ТОЛЬКО по паре «строка заголовка + строка-разделитель».
 * Текст с вертикальными чертами без разделителя таблицей не является и
 * остаётся абзацем — иначе любая ASCII-схема в ответе модели превращалась бы
 * в таблицу.
 */
function isTableStart(lines, i) {
    if (i + 1 >= lines.length) return false;
    if (!lines[i].includes('|')) return false;
    return isDelimiterRow(lines[i + 1]);
}

function isDelimiterRow(line) {
    if (!line || !line.includes('|')) return false;
    const cells = splitRow(line);
    if (cells.length === 0) return false;
    return cells.every((c) => /^:?-+:?$/.test(c.trim()));
}

/**
 * Делит строку таблицы на ячейки по НЕэкранированным `|`.
 *
 * Ручной проход, а не `split`, ровно из-за `\|`: экранированная черта обязана
 * остаться символом внутри ячейки, а не разрезать её.
 */
function splitRow(line) {
    let s = line.trim();
    if (s.startsWith('|')) s = s.slice(1);
    if (s.endsWith('|') && !s.endsWith('\\|')) s = s.slice(0, -1);
    const cells = [];
    let buf = '';
    for (let i = 0; i < s.length; i += 1) {
        const ch = s[i];
        if (ch === '\\' && s[i + 1] === '|') { buf += '|'; i += 1; continue; }
        if (ch === '|') { cells.push(buf); buf = ''; continue; }
        buf += ch;
    }
    cells.push(buf);
    return cells;
}

function alignOf(spec) {
    const s = spec.trim();
    const left = s.startsWith(':');
    const right = s.endsWith(':');
    if (left && right) return 'center';
    if (right) return 'right';
    if (left) return 'left';
    return null;
}

function cell(text) {
    const t = text.trim();
    return { text: t, inline: parseInline(t) };
}

/**
 * Читает таблицу. Строки с числом ячеек, отличным от заголовка, добиваются
 * пустыми или обрезаются: рваная таблица от модели — норма, а не повод
 * потерять данные или уронить разбор.
 */
function readTable(lines, start) {
    const header = splitRow(lines[start]).map(cell);
    const align = splitRow(lines[start + 1]).map(alignOf);
    while (align.length < header.length) align.push(null);
    align.length = header.length;

    const rows = [];
    let i = start + 2;
    while (i < lines.length && !isBlank(lines[i]) && lines[i].includes('|')) {
        const cells = splitRow(lines[i]).map(cell);
        while (cells.length < header.length) cells.push(cell(''));
        cells.length = header.length;
        rows.push(cells);
        i += 1;
    }
    return { node: { type: 'table', align, header, rows }, next: i };
}

// ==========================================================================
// Инлайн-парсер
// ==========================================================================

/**
 * Разбирает инлайн-содержимое одной логической строки (абзац, заголовок,
 * пункт списка, ячейка таблицы).
 *
 * Порядок правил не случаен:
 *   1. экранирование `\*` — раньше всего, иначе экранировать нечем;
 *   2. `` `код` `` — НАИВЫСШИЙ приоритет разметки: внутри кода ничего не
 *      разбирается (`` `**не жирный**` `` показывается как есть);
 *   3. `**`/`~~` — парные двойные маркеры;
 *   4. `*`/`_` — курсив;
 *   5. `[текст](url)` — ссылка; URL кладётся в AST СЫРЫМ, парсер про
 *      безопасность не знает вовсе (это дело `safeHref` и рендерера).
 *
 * Незакрытый маркер остаётся обычным текстом и НЕ съедает остаток строки:
 * `**без пары` — это девять символов текста, а не начало жирного до конца
 * ответа. Строка из одних маркеров (`***`, `~~~~`) разбирается за один проход
 * и не зацикливается — каждый шаг цикла двигает `i` минимум на один символ.
 *
 * ЧИСТАЯ функция: DOM не трогает.
 *
 * @param {string} src
 * @param {number} [depth] — защита от бесконечной рекурсии на вложенности
 * @returns {Inline[]}
 */
export function parseInline(src, depth = 0) {
    const out = [];
    if (typeof src !== 'string' || src === '') return out;
    if (depth > MAX_NESTING) return [{ type: 'text', value: src }];

    let buf = '';
    let i = 0;
    const flush = () => {
        if (buf !== '') { out.push({ type: 'text', value: buf }); buf = ''; }
    };

    while (i < src.length) {
        const ch = src[i];

        if (ch === '\\' && i + 1 < src.length && ESCAPABLE.includes(src[i + 1])) {
            buf += src[i + 1];
            i += 2;
            continue;
        }

        if (ch === '`') {
            const run = /^`+/.exec(src.slice(i))[0];
            const close = src.indexOf(run, i + run.length);
            // Закрытие ищется той же длины: ``a`b`` — один код с бэктиком.
            if (close !== -1) {
                flush();
                out.push({ type: 'code', value: src.slice(i + run.length, close) });
                i = close + run.length;
                continue;
            }
            buf += run;
            i += run.length;
            continue;
        }

        if (src.startsWith('**', i)) {
            const node = readDelimited(src, i, '**', 'strong', depth);
            if (node) { flush(); out.push(node.node); i = node.next; continue; }
        }

        if (src.startsWith('~~', i)) {
            const node = readDelimited(src, i, '~~', 'strike', depth);
            if (node) { flush(); out.push(node.node); i = node.next; continue; }
        }

        if (ch === '*' || ch === '_') {
            // `_` не работает внутри слова: snake_case_имена в ответах модели
            // встречаются чаще, чем подчёркнутый курсив посреди слова.
            const prev = i > 0 ? src[i - 1] : '';
            const wordBefore = ch === '_' && /[\p{L}\p{N}]/u.test(prev);
            if (!wordBefore) {
                const node = readDelimited(src, i, ch, 'em', depth);
                if (node) {
                    const after = src[node.next] || '';
                    const wordAfter = ch === '_' && /[\p{L}\p{N}]/u.test(after);
                    if (!wordAfter) { flush(); out.push(node.node); i = node.next; continue; }
                }
            }
        }

        if (ch === '[') {
            const link = readLink(src, i, depth);
            if (link) { flush(); out.push(link.node); i = link.next; continue; }
        }

        buf += ch;
        i += 1;
    }

    flush();
    return out;
}

/**
 * Общая механика парных маркеров (`**`, `~~`, `*`, `_`).
 * Возвращает null, если пары нет или содержимое пустое/начинается с пробела —
 * в этом случае вызывающий добавит символ в текст как есть.
 */
function readDelimited(src, start, marker, type, depth) {
    const from = start + marker.length;
    let at = from;
    while (at < src.length) {
        const idx = src.indexOf(marker, at);
        if (idx === -1) return null;
        // Экранированный закрывающий маркер не закрывает.
        if (src[idx - 1] === '\\') { at = idx + marker.length; continue; }
        const inner = src.slice(from, idx);
        // Содержимое из одних маркеров (`***`, `~~~~`) — не разметка, а текст:
        // иначе `***` давал бы курсив из одной звёздочки.
        const onlyMarkers = inner !== '' && inner.split('').every((c) => c === marker[0]);
        if (inner === '' || onlyMarkers || /^\s/.test(inner) || /\s$/.test(inner)) {
            at = idx + marker.length;
            continue;
        }
        return {
            node: { type, children: parseInline(inner, depth + 1) },
            next: idx + marker.length,
        };
    }
    return null;
}

/**
 * Ссылка `[текст](url)`.
 *
 * Скобки внутри считаются с учётом вложенности и экранирования: `[a[b]c](u)` и
 * `[t](http://x/(y))` разбираются целиком. Незакрытая конструкция (`[текст](`)
 * — обычный текст.
 *
 * URL сохраняется СЫРЫМ, включая пробелы и переводы строк: нормализовать его
 * будет `new URL` в `safeHref`, и подрезать строку заранее нельзя — именно
 * подрезка «до первого пробела» и есть классический способ пронести
 * `java\nscript:` мимо проверки.
 */
function readLink(src, start, depth) {
    let i = start + 1;
    let depthBr = 1;
    while (i < src.length && depthBr > 0) {
        const ch = src[i];
        if (ch === '\\') { i += 2; continue; }
        if (ch === '[') depthBr += 1;
        else if (ch === ']') depthBr -= 1;
        if (depthBr === 0) break;
        i += 1;
    }
    if (depthBr !== 0 || src[i + 1] !== '(') return null;
    const text = src.slice(start + 1, i);

    let j = i + 2;
    let depthPar = 1;
    while (j < src.length && depthPar > 0) {
        const ch = src[j];
        if (ch === '\\') { j += 2; continue; }
        if (ch === '(') depthPar += 1;
        else if (ch === ')') depthPar -= 1;
        if (depthPar === 0) break;
        j += 1;
    }
    if (depthPar !== 0) return null;
    const href = src.slice(i + 2, j);

    return {
        node: { type: 'link', href, children: parseInline(text, depth + 1) },
        next: j + 1,
    };
}

// ==========================================================================
// Безопасность ссылок
// ==========================================================================

/**
 * Разрешённые схемы. Всё остальное — включая `data:` (можно спрятать целый
 * HTML-документ) и `about:` — запрещено.
 */
const ALLOWED_PROTOCOLS = ['http:', 'https:'];

/**
 * Единственное место фичи, где недоверенный текст становится КЛИКАБЕЛЬНЫМ
 * элементом. Всё остальное в модуле — текстовые узлы; ссылка — узел с
 * атрибутом `href`, который браузер выполнит.
 *
 * Решение проверять через `new URL`, а НЕ регекспом по началу строки:
 * префиксный регексп обходится ведущими пробелами, табами, переводами строк,
 * управляющими символами и регистром (` javascript:`, `java\nscript:`,
 * `JaVaScRiPt:`). Конструктор WHATWG-URL выполняет ровно ту же нормализацию,
 * что и браузер при переходе по ссылке — он вырезает табы и переводы строк
 * внутри URL, обрезает пробелы по краям и приводит схему к нижнему регистру.
 * Сравнивать после этого нужно уже не с текстом, а с `url.protocol`, и тогда
 * все перечисленные обходы схлопываются в одно и то же `javascript:`.
 *
 * ОТНОСИТЕЛЬНЫЕ ССЫЛКИ ЗАПРЕЩЕНЫ (решение явное): базы у нас нет, а
 * подставлять `location.href` панели значило бы позволить ответу модели
 * сделать кликабельной ссылку на собственный API devforge (`/api/vk/...`) с
 * куками пользователя. `new URL(raw)` без базы бросает на относительном пути
 * — и такая ссылка становится текстом.
 *
 * @param {*} raw — URL из AST, как его написала модель
 * @returns {?string} — нормализованный абсолютный URL или null, если ссылку
 *                      нельзя делать кликабельной
 */
export function safeHref(raw) {
    if (typeof raw !== 'string' || raw === '') return null;
    let url;
    try {
        url = new URL(raw);
    } catch (_e) {
        // Не абсолютный URL или синтаксический мусор — обоим место в тексте.
        return null;
    }
    if (!ALLOWED_PROTOCOLS.includes(url.protocol)) return null;
    return url.href;
}

// ==========================================================================
// Рендерер
// ==========================================================================

/**
 * Собирает DocumentFragment из AST.
 *
 * `doc` передаётся ПАРАМЕТРОМ, а не берётся из глобального `document`: это и
 * делает рендерер проверяемым на фейковом документе в Node (см. секцию 2
 * теста messenger_markdown.test.mjs). Узлы создаются исключительно через
 * `createElement`/`createTextNode`/`appendChild`/`setAttribute` — см. инвариант
 * в шапке файла.
 *
 * Каждый вызов строит НОВЫЕ узлы: один и тот же AST можно отрисовать дважды в
 * два разных места, и они не будут делить ноды.
 *
 * @param {Block[]} ast
 * @param {Document} doc
 * @returns {DocumentFragment}
 */
export function renderMarkdown(ast, doc) {
    const frag = doc.createDocumentFragment();
    if (!Array.isArray(ast)) return frag;
    for (const block of ast) {
        const node = renderBlock(block, doc);
        if (node) frag.appendChild(node);
    }
    return frag;
}

/**
 * Разобрать и отрисовать одним вызовом — то, что нужно вызывающему коду
 * панели: `out.appendChild(renderMarkdownText(answer, document))`.
 *
 * @param {string} text
 * @param {Document} doc
 * @returns {DocumentFragment}
 */
export function renderMarkdownText(text, doc) {
    return renderMarkdown(parseMarkdown(text), doc);
}

/** Язык блока кода уходит в data-атрибут; мусорные значения отбрасываются. */
function safeLang(lang) {
    if (typeof lang !== 'string') return null;
    return /^[A-Za-z0-9_+#.-]{1,24}$/.test(lang) ? lang.toLowerCase() : null;
}

function renderBlock(block, doc) {
    if (!block || typeof block !== 'object') return null;

    switch (block.type) {
        case 'paragraph': {
            const p = doc.createElement('p');
            p.className = 'md-p';
            appendInline(p, block.inline, doc);
            return p;
        }
        case 'heading': {
            const level = Math.min(6, Math.max(1, Number(block.level) || 1));
            const h = doc.createElement(`h${level}`);
            h.className = 'md-h';
            appendInline(h, block.inline, doc);
            return h;
        }
        case 'hr': {
            const hr = doc.createElement('hr');
            hr.className = 'md-hr';
            return hr;
        }
        case 'code': {
            const pre = doc.createElement('pre');
            pre.className = 'md-pre';
            const code = doc.createElement('code');
            code.className = 'md-code-block';
            const lang = safeLang(block.lang);
            // Подсветки нет намеренно (решение ТЗ) — язык сохранён как
            // атрибут, чтобы её можно было добавить позже без правки парсера.
            if (lang) code.setAttribute('data-lang', lang);
            code.appendChild(doc.createTextNode(String(block.text == null ? '' : block.text)));
            pre.appendChild(code);
            return pre;
        }
        case 'blockquote': {
            const bq = doc.createElement('blockquote');
            bq.className = 'md-quote';
            for (const inner of block.blocks || []) {
                const node = renderBlock(inner, doc);
                if (node) bq.appendChild(node);
            }
            return bq;
        }
        case 'list':
            return renderList(block, doc);
        case 'table':
            return renderTable(block, doc);
        default:
            return null;
    }
}

function renderList(block, doc) {
    const list = doc.createElement(block.ordered ? 'ol' : 'ul');
    list.className = block.ordered ? 'md-ol' : 'md-ul';
    // Нумерация с произвольного числа: `3. первый пункт` обязан остаться
    // третьим, иначе ссылки вида «см. пункт 5» в ответе модели врут.
    if (block.ordered && Number.isFinite(block.start) && block.start !== 1) {
        list.setAttribute('start', String(block.start));
    }
    for (const item of block.items || []) {
        const li = doc.createElement('li');
        li.className = 'md-li';
        appendInline(li, item.inline, doc);
        for (const child of item.children || []) {
            const node = renderBlock(child, doc);
            if (node) li.appendChild(node);
        }
        list.appendChild(li);
    }
    return list;
}

function renderTable(block, doc) {
    // Обёртка со СВОИМ горизонтальным скроллом: широкая таблица обязана
    // скроллиться внутри себя, а не растягивать попап разбора (правило ТЗ).
    const wrap = doc.createElement('div');
    wrap.className = 'md-table-wrap';

    const table = doc.createElement('table');
    table.className = 'md-table';

    const thead = doc.createElement('thead');
    const htr = doc.createElement('tr');
    (block.header || []).forEach((c, idx) => {
        const th = doc.createElement('th');
        applyAlign(th, block.align && block.align[idx]);
        appendInline(th, c.inline, doc);
        htr.appendChild(th);
    });
    thead.appendChild(htr);
    table.appendChild(thead);

    const tbody = doc.createElement('tbody');
    for (const row of block.rows || []) {
        const tr = doc.createElement('tr');
        row.forEach((c, idx) => {
            const td = doc.createElement('td');
            applyAlign(td, block.align && block.align[idx]);
            appendInline(td, c.inline, doc);
            tr.appendChild(td);
        });
        tbody.appendChild(tr);
    }
    table.appendChild(tbody);
    wrap.appendChild(table);
    return wrap;
}

/** Выравнивание — стилем, а не устаревшим атрибутом `align`. */
function applyAlign(node, align) {
    if (align === 'left' || align === 'center' || align === 'right') {
        node.style.textAlign = align;
    }
}

function appendInline(parent, inline, doc) {
    if (!Array.isArray(inline)) return;
    for (const node of inline) {
        const el = renderInline(node, doc);
        if (el) parent.appendChild(el);
    }
}

function renderInline(node, doc) {
    if (!node || typeof node !== 'object') return null;
    switch (node.type) {
        case 'text':
            return doc.createTextNode(String(node.value == null ? '' : node.value));
        case 'code': {
            const code = doc.createElement('code');
            code.className = 'md-code';
            code.appendChild(doc.createTextNode(String(node.value == null ? '' : node.value)));
            return code;
        }
        case 'strong': {
            const el = doc.createElement('strong');
            appendInline(el, node.children, doc);
            return el;
        }
        case 'em': {
            const el = doc.createElement('em');
            appendInline(el, node.children, doc);
            return el;
        }
        case 'strike': {
            const el = doc.createElement('s');
            appendInline(el, node.children, doc);
            return el;
        }
        case 'link':
            return renderLink(node, doc);
        default:
            return null;
    }
}

/**
 * Ссылка. Если схема не прошла белый список — элемент `<a>` НЕ создаётся
 * вовсе: текст ссылки отдаётся обычным текстовым узлом, и пользователь видит
 * всё, что написала модель, просто без клика. Пустой `href` или `<a>` без
 * адреса были бы хуже: ссылка выглядела бы рабочей.
 */
function renderLink(node, doc) {
    const href = safeHref(node.href);
    if (!href) {
        const span = doc.createElement('span');
        span.className = 'md-link-blocked';
        appendInline(span, node.children, doc);
        // Совсем без текста ссылка исчезла бы с экрана — показываем сырой URL.
        if (!span.childNodes || span.childNodes.length === 0) {
            span.appendChild(doc.createTextNode(String(node.href == null ? '' : node.href)));
        }
        return span;
    }
    const a = doc.createElement('a');
    a.className = 'md-link';
    a.setAttribute('href', href);
    // target/rel — оба обязательны: без rel новая вкладка получает
    // window.opener и может увести исходную страницу на фишинг.
    a.setAttribute('target', '_blank');
    a.setAttribute('rel', 'noopener noreferrer');
    appendInline(a, node.children, doc);
    if (!a.childNodes || a.childNodes.length === 0) a.appendChild(doc.createTextNode(href));
    return a;
}
