// Регресс-тест читаемости приглушённого текста панели VK (css/vk.css).
//
// ЧТО ЗА ДЕФЕКТ. Пользовательская приёмка: «в тёмной теме не видно сообщение,
// на которое ответил пользователь». Текст цитаты (`.vk-reply-quote-text`)
// красится токеном `--v-faint-text`, а тот в тёмной теме был объявлен как
// `var(--v-acc-faint)` — то есть цветом РАМОК (#12283F, рядом в файле стоит
// комментарий «1.4:1 — РАМКИ, не текст»). На фоне панели это давало 1.3:1, а
// на фоне цитаты под курсором (`--v-active`) — 1.12:1, то есть текст
// физически невидим.
//
// Тем же токеном красятся ещё девять мест: метка времени строки, приписка
// «(edited)», хвост «…ещё N пересланных», удалённое сообщение, разделитель
// дат, шапка клавиатуры сообщества, превью в полосе ответа и две строки
// шпаргалки клавиш. Поэтому тест проверяет ТОКЕН, а не одно правило: починка
// обязана вылечить все места разом и ни одно не сделать хуже.
//
// ПОЧЕМУ ЧИТАЕТСЯ САМ CSS. Node-стенд без браузера каскад не считает, а
// подставлять числа руками означало бы проверять копию, а не файл: разошлись
// бы они молча. Здесь из `css/vk.css` вынимаются блоки объявлений токенов и
// значения сверяются формулой WCAG из общего слоя
// (`js/messenger/theme-core.js::contrastRatio`) — третьей копии математики
// цвета в проекте не заводится.
//
// Запуск: node tmux-web/tests/frontend/vk_theme_contrast.test.mjs

import { readFileSync } from 'node:fs';

const { contrastRatio, parseCssColor } = await import(
    new URL('../../static/js/messenger/theme-core.js', import.meta.url).href
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
    if (JSON.stringify(actual) !== JSON.stringify(expected)) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${JSON.stringify(expected)}`
            + `\n       получено:  ${JSON.stringify(actual)}`);
    }
}

/** Порог WCAG для мелкого текста. Цитата — 11px, послаблений нет. */
const AA_TEXT = 4.5;

const CSS_PATH = new URL('../../static/css/vk.css', import.meta.url);
const css = readFileSync(CSS_PATH, 'utf8');

/**
 * Объявления токенов из одного блока правил.
 *
 * Селектор ищется как есть, тело берётся до первой закрывающей скобки:
 * вложенных блоков внутри объявления палитры не бывает, а полноценный парсер
 * CSS здесь был бы отдельной библиотекой с собственными багами.
 *
 * @param {string} selector
 * @returns {Object<string, string>} `--v-bg` → `#050709`
 */
function tokensOf(selector) {
    const at = css.indexOf(`${selector} {`);
    if (at < 0) throw new Error(`блок «${selector}» не найден в css/vk.css`);
    const body = css.slice(at, css.indexOf('\n}', at));
    const out = {};
    for (const m of body.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) {
        out[m[1]] = m[2].trim();
    }
    return out;
}

/** Разворачивает `var(--x)` по таблице токенов (одного уровня достаточно). */
function resolve(tokens, name) {
    const raw = tokens[name];
    if (raw === undefined) return null;
    const ref = /^var\(\s*(--[a-z0-9-]+)\s*\)$/.exec(raw);
    return ref ? (tokens[ref[1]] ?? null) : raw;
}

const dark = tokensOf('#vk');
const light = tokensOf('#vk[data-vk-theme="light"]');

// ---------------------------------------------------------------------------
// 1. Структурный щит: приглушённый ТЕКСТ — не цвет РАМОК
// ---------------------------------------------------------------------------

for (const [name, tokens] of [['тёмная', dark], ['светлая', light]]) {
    const faintRaw = tokens['--v-faint-text'];
    ok(faintRaw !== undefined, `${name} тема: токен --v-faint-text объявлен`);
    ok(!/^var\(\s*--v-acc-faint\s*\)$/.test(String(faintRaw)),
        `${name} тема: --v-faint-text не должен быть псевдонимом рамочного`
        + ` --v-acc-faint (сейчас «${faintRaw}»)`);
    ok(resolve(tokens, '--v-faint-text') !== resolve(tokens, '--v-acc-faint'),
        `${name} тема: значение --v-faint-text не совпадает с --v-acc-faint`);
}

// ---------------------------------------------------------------------------
// 2. Тёмная тема: 4.5:1 на всех фонах, где этот текст встречается
// ---------------------------------------------------------------------------

/**
 * Фоны, на которых реально лежит текст цвета --v-faint-text.
 *
 * `--v-active` обязателен наравне с остальными: это фон `.vk-reply-quote` под
 * курсором и в фокусе, и без него дефект вернулся бы в hover-состоянии.
 */
const BACKDROPS = ['--v-bg', '--v-panel', '--v-bar', '--v-active'];

const faintDark = resolve(dark, '--v-faint-text');
ok(parseCssColor(faintDark) !== null,
    `тёмная тема: --v-faint-text разбирается как цвет (${faintDark})`);

for (const bg of BACKDROPS) {
    const ratio = contrastRatio(faintDark, resolve(dark, bg));
    ok(ratio !== null && ratio >= AA_TEXT,
        `тёмная тема: --v-faint-text (${faintDark}) против ${bg}`
        + ` (${resolve(dark, bg)}) = ${ratio === null ? 'не разобрано' : ratio.toFixed(2)}:1,`
        + ` нужно ≥ ${AA_TEXT}:1`);
}

// ---------------------------------------------------------------------------
// 3. Иерархия сохранена: приглушённый текст не ярче второстепенного
// ---------------------------------------------------------------------------

{
    // «Автор ярче превью» (комментарий у .vk-reply-quote-name): имя красится
    // --v-acc-dim, текст — --v-faint-text. По контрасту к фону приглушённый
    // текст не имеет права обогнать ни --v-acc-dim, ни тем более акцент.
    const dim = contrastRatio(resolve(dark, '--v-acc-dim'), resolve(dark, '--v-bg'));
    const acc = contrastRatio(resolve(dark, '--v-acc'), resolve(dark, '--v-bg'));
    const faint = contrastRatio(faintDark, resolve(dark, '--v-bg'));
    ok(faint <= dim + 1e-9,
        `тёмная тема: приглушённый текст (${faint.toFixed(2)}:1) не ярче`
        + ` --v-acc-dim (${dim.toFixed(2)}:1)`);
    ok(faint < acc,
        `тёмная тема: приглушённый текст (${faint.toFixed(2)}:1) тише акцента`
        + ` --v-acc (${acc.toFixed(2)}:1)`);
}

// ---------------------------------------------------------------------------
// 4. Светлая тема: не деградировала
// ---------------------------------------------------------------------------

{
    // Светлую палитру эта задача не трогает: там токен и так объявлен своим
    // значением. Щит держит ровно то, что есть, — чтобы «починка» тёмной темы
    // не увезла светлую обратно к рамочному цвету.
    const faintLight = resolve(light, '--v-faint-text');
    ok(parseCssColor(faintLight) !== null,
        `светлая тема: --v-faint-text разбирается как цвет (${faintLight})`);
    const ratio = contrastRatio(faintLight, resolve(light, '--v-bg'));
    ok(ratio !== null && ratio >= 3.5,
        `светлая тема: --v-faint-text против --v-bg = ${ratio === null ? 'не разобрано' : ratio.toFixed(2)}:1`);
}

// ---------------------------------------------------------------------------
// 5. Сама математика: контрольные точки формулы WCAG
// ---------------------------------------------------------------------------

{
    eq(Math.round(contrastRatio('#FFFFFF', '#000000')), 21, 'белое к чёрному — 21:1');
    eq(Math.round(contrastRatio('#000000', '#000000')), 1, 'цвет сам с собой — 1:1');
    eq(contrastRatio('oklch(0.5 0.1 200)', '#000000'), null,
        'нераспознанный цвет даёт null, а не число из мусора');
    // Порядок аргументов не меняет ответа: отношение считается большим к меньшему.
    eq(
        contrastRatio('#12283F', '#070B10').toFixed(4),
        contrastRatio('#070B10', '#12283F').toFixed(4),
        'отношение симметрично',
    );
}

console.log(`vk_theme_contrast: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
