// ВСПОМОГАТЕЛЬНЫЙ МОДУЛЬ (не тест): проверка CSS-СЕЛЕКТОРОВ ПРОТИВ НАСТОЯЩЕГО
// УЗЛА, собранного панелью.
//
// Зачем он существует. Кнопка ответа в Telegram и VK показывалась правилом с
// ДОЧЕРНИМ комбинатором (`.tg-line:hover > .tg-reply-btn`). Когда кнопка
// переехала внутрь обёртки `span.tg-line-actions`, правило перестало
// совпадать, и кнопка исчезла бы МОЛЧА: в консоли пусто, а тесты, которые
// ищут её рекурсивным `querySelector`, остаются зелёными — они проверяют
// наличие узла, а не то, что его будет видно.
//
// Поэтому страж устроен иначе: он берёт СЕЛЕКТОРЫ ИЗ САМОГО CSS-ФАЙЛА и
// проверяет, совпадают ли они с узлом, который панель реально построила. Верни
// кто-нибудь дочерний комбинатор — тест краснеет, потому что узел больше не
// прямой потомок строки.
//
// Матчер намеренно крошечный: он понимает ровно то, чем пользуются панели, —
// компаунды из тега, классов и атрибутов, комбинаторы ` ` и `>`, а
// псевдоклассы состояния (`:hover`, `:focus-within`, `:focus-visible`)
// считает выполненными. Это и есть постановка вопроса: «наведись пользователь
// на строку, увидел бы он кнопку?».

import { readFileSync } from 'node:fs';

/** Прочитать CSS-файл рядом с тестом. */
export function readCss(url, rel) {
    return readFileSync(new URL(rel, url), 'utf8');
}

/** CSS без комментариев: в шапках файлов селекторы упоминаются прозой. */
export function stripComments(css) {
    return String(css).replace(/\/\*[\s\S]*?\*\//g, '');
}

/**
 * Все селекторы файла, упоминающие подстроку.
 *
 * Возвращает ОДИНОЧНЫЕ селекторы (список через запятую уже разобран), потому
 * что совпадение проверяется по одному за раз.
 *
 * @param {string} css
 * @param {string} needle — например `.tg-reply-btn`
 * @returns {Array<string>}
 */
export function selectorsWith(css, needle) {
    const code = stripComments(css);
    const out = [];
    // Блоки правил: всё до `{`, что не является телом другого правила.
    const re = /([^{}]+)\{([^{}]*)\}/g;
    let m;
    while ((m = re.exec(code)) !== null) {
        const head = m[1].trim();
        // `@media …` и прочие at-правила: их «селектор» — это условие, а
        // вложенные правила ловятся следующими итерациями (тело @media само
        // состоит из блоков).
        if (head.startsWith('@')) continue;
        for (const sel of head.split(',')) {
            const one = sel.trim();
            if (one && one.includes(needle)) out.push(one);
        }
    }
    return out;
}

/**
 * Тело правила по точному селектору (первое совпадение).
 *
 * @param {string} css
 * @param {string} selector — как он записан в файле
 * @returns {?string}
 */
export function ruleBody(css, selector) {
    const code = stripComments(css);
    const re = /([^{}]+)\{([^{}]*)\}/g;
    let m;
    while ((m = re.exec(code)) !== null) {
        const head = m[1].trim();
        if (head.startsWith('@')) continue;
        const parts = head.split(',').map((s) => s.trim());
        if (parts.includes(selector)) return m[2];
    }
    return null;
}

/** Объявлено ли свойство в теле правила. */
export function declares(body, prop, value) {
    if (typeof body !== 'string') return false;
    const re = new RegExp(`(^|[;{\\s])${prop}\\s*:\\s*([^;]+)`, 'i');
    const m = re.exec(body);
    if (!m) return false;
    if (value === undefined) return true;
    return m[2].trim().toLowerCase().startsWith(String(value).toLowerCase());
}

// ---------------------------------------------------------------------------
// Матчер
// ---------------------------------------------------------------------------

/** Псевдоклассы состояния, которые считаются выполненными. */
const STATE_PSEUDO = new Set([
    'hover', 'focus', 'focus-within', 'focus-visible', 'active', 'first-child', 'last-child',
]);

/**
 * Разбор компаунда: `div.a.b[data-x="1"]:hover` → части.
 *
 * @returns {?{tag: string, classes: Array<string>, attrs: Array<object>}}
 *   null — компаунд содержит то, чего матчер не понимает (например `::before`
 *   или `:not(...)`): тогда правило проверять нечем, и вызывающий его
 *   пропускает, а не считает совпавшим.
 */
function parseCompound(text) {
    let rest = String(text);
    if (rest.includes('::')) return null;
    const out = { tag: '', classes: [], attrs: [] };
    while (rest.length > 0) {
        const ch = rest[0];
        if (ch === '.') {
            const m = /^\.([-\w]+)/.exec(rest);
            if (!m) return null;
            out.classes.push(m[1]);
            rest = rest.slice(m[0].length);
            continue;
        }
        if (ch === '[') {
            const m = /^\[([-\w]+)(?:([~|^$*]?=)"?([^\]"]*)"?)?\]/.exec(rest);
            if (!m) return null;
            out.attrs.push({ name: m[1], op: m[2] || null, value: m[3] });
            rest = rest.slice(m[0].length);
            continue;
        }
        if (ch === ':') {
            const m = /^:([-\w]+)/.exec(rest);
            if (!m || !STATE_PSEUDO.has(m[1])) return null;
            rest = rest.slice(m[0].length);
            continue;
        }
        const m = /^([-\w]+|\*)/.exec(rest);
        if (!m) return null;
        out.tag = m[1] === '*' ? '' : m[1].toLowerCase();
        rest = rest.slice(m[0].length);
    }
    return out;
}

/** Атрибуты узла: и настоящие, и dataset (панели пишут именно в dataset). */
function attrValue(node, name) {
    if (typeof node.getAttribute === 'function') {
        const v = node.getAttribute(name);
        if (v !== null && v !== undefined) return String(v);
    }
    if (node.attributes && name in node.attributes) return String(node.attributes[name]);
    const data = /^data-(.+)$/.exec(name);
    if (data && node.dataset) {
        const camel = data[1].replace(/-([a-z])/g, (_, c) => c.toUpperCase());
        if (camel in node.dataset) return String(node.dataset[camel]);
    }
    if (name === 'hidden' && node.hidden === true) return '';
    return null;
}

function matchesCompound(node, compound) {
    if (!node || node.nodeType !== 1) return false;
    if (compound.tag && String(node.tagName).toLowerCase() !== compound.tag) return false;
    for (const c of compound.classes) {
        const has = node.classList && typeof node.classList.contains === 'function'
            ? node.classList.contains(c)
            : String(node.className || '').split(/\s+/).includes(c);
        if (!has) return false;
    }
    for (const a of compound.attrs) {
        const v = attrValue(node, a.name);
        if (v === null) return false;
        if (a.op === '=' && v !== a.value) return false;
        if (a.op === '*=' && !v.includes(a.value)) return false;
        if (a.op === '^=' && !v.startsWith(a.value)) return false;
    }
    return true;
}

/**
 * Совпадает ли селектор с узлом.
 *
 * @param {string} selector — одиночный (без запятых)
 * @param {object} node — узел фейкового DOM или браузерный
 * @returns {?boolean} null — селектор матчеру не по зубам
 */
export function selectorMatches(selector, node) {
    const raw = String(selector).trim();
    if (!raw || raw.includes('~') || raw.includes('+') || raw.includes(':not')) return null;
    // Разбор справа налево: последний компаунд обязан совпасть с самим узлом.
    const tokens = raw.split(/\s*(>)\s*|\s+/).filter((t) => t !== undefined && t !== '');
    const parts = [];
    for (const t of tokens) parts.push(t);
    let idx = parts.length - 1;
    const self = parseCompound(parts[idx]);
    if (!self) return null;
    if (!matchesCompound(node, self)) return false;
    idx -= 1;
    let cur = node;
    while (idx >= 0) {
        let combinator = ' ';
        if (parts[idx] === '>') {
            combinator = '>';
            idx -= 1;
            if (idx < 0) return null;
        }
        const compound = parseCompound(parts[idx]);
        if (!compound) return null;
        if (combinator === '>') {
            cur = cur.parentNode;
            if (!cur || !matchesCompound(cur, compound)) return false;
        } else {
            let up = cur.parentNode;
            let found = null;
            while (up) {
                if (matchesCompound(up, compound)) { found = up; break; }
                up = up.parentNode;
            }
            if (!found) return false;
            cur = found;
        }
        idx -= 1;
    }
    return true;
}

/**
 * Совпадает ли с узлом ХОТЬ ОДИН из селекторов правила показа.
 *
 * @param {Array<string>} selectors
 * @param {object} node
 * @returns {boolean}
 */
export function anySelectorMatches(selectors, node) {
    return selectors.some((s) => selectorMatches(s, node) === true);
}
