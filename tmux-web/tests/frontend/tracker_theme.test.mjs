// Регресс-щит светлой палитры панели трекера (css/tracker.css).
//
//   node --test tmux-web/tests/frontend/tracker_theme.test.mjs
//
// ЧТО ЗДЕСЬ ОХРАНЯЕТСЯ. У панели #tracker собственная палитра: токены `--t-*`
// объявлены на `#tracker` (тёмная) и на `#tracker[data-tracker-theme="light"]`
// (светлая). Разъехаться они могут молча — новый токен добавили в тёмный
// блок, в светлый забыли, и на светлой теме элемент наследует фосфорный
// `#33FF88`, который на белом даёт 1.30:1, то есть невидим. Поэтому тест
// сверяет НАБОРЫ токенов, а не отдельные правила.
//
// ПОЧЕМУ ЧИТАЕТСЯ САМ CSS. Node-стенд без браузера каскад не считает, а
// вписать числа руками означало бы проверять копию, а не файл. Формула WCAG
// берётся из общего слоя (`js/messenger/theme-core.js::contrastRatio`) —
// четвёртой копии математики цвета в проекте не заводится. Разбор блока
// объявлений — тот же приём, что в tests/frontend/vk_theme_contrast.test.mjs.

import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';

import { contrastRatio, parseCssColor } from '../../static/js/messenger/theme-core.js';

/** Порог WCAG для мелкого текста. Панель набрана 12px, послаблений нет. */
const AA_TEXT = 4.5;

const css = readFileSync(new URL('../../static/css/tracker.css', import.meta.url), 'utf8');

/**
 * Объявления токенов из одного блока правил.
 *
 * Селектор ищется как есть, тело берётся до первой закрывающей скобки:
 * вложенных блоков внутри объявления палитры не бывает, а полноценный парсер
 * CSS был бы отдельной библиотекой с собственными багами.
 *
 * @param {string} selector
 * @returns {Object<string, string>} `--t-bg` → `#050805`
 */
function tokensOf(selector) {
    const at = css.indexOf(`${selector} {`);
    assert.ok(at >= 0, `блок «${selector}» не найден в css/tracker.css`);
    const body = css.slice(at, css.indexOf('\n}', at));
    const out = {};
    for (const m of body.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) out[m[1]] = m[2].trim();
    return out;
}

const dark = tokensOf('#tracker');
const light = tokensOf('#tracker[data-tracker-theme="light"]');

/** Фоны панели. `--t-active` (подсветка строки) режет пороги сильнее прочих. */
const BACKDROPS = ['--t-bg', '--t-panel', '--t-bar', '--t-active'];
/**
 * Порог для пары «текст на фоне».
 *
 * Светлая палитра держит 4.5:1 ВЕЗДЕ, включая подсветку активной строки —
 * она подбиралась под этот порог, и опускать его нельзя.
 *
 * У ТЁМНОЙ палитры на `--t-active` (#0E241A) исторически 4.03:1 у
 * `--t-acc-dim` и `--t-muted`: значения пришли из макета ForgeTracker.pen и
 * этой задачей НЕ трогаются — перекрашивать существующий вид панели ради
 * ретроспективного порога значило бы менять то, что уже принято глазами.
 * Щит фиксирует достигнутое (4.0), чтобы дальше не ухудшалось, а не
 * притворяется, что там 4.5.
 *
 * @param {string} theme — 'тёмная' | 'светлая'
 * @param {string} bg — имя токена фона
 * @returns {number}
 */
function floorFor(theme, bg) {
    if (theme === 'тёмная' && bg === '--t-active') return 4.0;
    return AA_TEXT;
}
/** Токены, которыми красится ТЕКСТ. `--t-acc-faint` сюда не входит — рамки. */
const TEXT_TOKENS = [
    '--t-acc', '--t-acc-dim', '--t-muted', '--t-text',
    '--t-warn', '--t-danger', '--t-ok',
];

test('светлая палитра переопределяет ВСЕ токены тёмной', () => {
    // Пропущенный токен наследуется из блока #tracker, то есть на светлом
    // фоне остаётся фосфорный — ровно тот дефект, ради которого тест и есть.
    const missing = Object.keys(dark).filter((k) => !(k in light));
    assert.deepEqual(missing, [], `в светлой палитре нет токенов: ${missing.join(', ')}`);
    // И наоборот: токен, которого нет в тёмной, на тёмной теме не существует.
    const extra = Object.keys(light).filter((k) => !(k in dark));
    assert.deepEqual(extra, [], `в тёмной палитре нет токенов: ${extra.join(', ')}`);
});

for (const [name, tokens] of [['тёмная', dark], ['светлая', light]]) {
    test(`${name} тема: весь текст читаем на всех четырёх фонах`, () => {
        for (const fg of TEXT_TOKENS) {
            for (const bg of BACKDROPS) {
                const ratio = contrastRatio(tokens[fg], tokens[bg]);
                assert.ok(ratio !== null, `${fg} (${tokens[fg]}) не разобран как цвет`);
                const min = floorFor(name, bg);
                assert.ok(
                    ratio >= min,
                    `${name}: ${fg} (${tokens[fg]}) на ${bg} (${tokens[bg]})`
                    + ` = ${ratio.toFixed(2)}:1, нужно >= ${min}:1`,
                );
            }
        }
    });

    test(`${name} тема: рамка и приглушённый текст — разные роли`, () => {
        // Слить их в один токен нельзя: рамке нужен цвет НИЖЕ текстового
        // порога, подписи — выше. В css/vk.css и css/telegram.css это уже
        // ломалось: приглушённый текст оказывался цветом рамок и исчезал.
        assert.notEqual(tokens['--t-acc-faint'], tokens['--t-muted']);
        const border = contrastRatio(tokens['--t-acc-faint'], tokens['--t-bg']);
        assert.ok(
            border < 2,
            `${name}: --t-acc-faint (${tokens['--t-acc-faint']}) = ${border.toFixed(2)}:1`
            + ' — это уже не рамка, а текст',
        );
    });

    test(`${name} тема: акцент заметнее приглушённой подписи`, () => {
        const acc = contrastRatio(tokens['--t-acc'], tokens['--t-bg']);
        const muted = contrastRatio(tokens['--t-muted'], tokens['--t-bg']);
        assert.ok(
            muted < acc,
            `${name}: подпись (${muted.toFixed(2)}:1) должна быть тише`
            + ` акцента (${acc.toFixed(2)}:1)`,
        );
    });

    test(`${name} тема: скрим и подсветка приёмника разбираются как цвет`, () => {
        // Оба были литералами внутри правил и не переживали смену темы:
        // чёрная плёнка 0.62 гасила светлую страницу, зелёная 0.04 на белом
        // не читалась вовсе. Теперь это токены — значит, обязаны быть цветом.
        for (const t of ['--t-scrim', '--t-drop']) {
            assert.ok(parseCssColor(tokens[t]) !== null, `${name}: ${t} = ${tokens[t]}`);
        }
    });
}

test('светлая тема: зелёный акцент уведён в читаемый тон', () => {
    // Фосфор тёмной темы на светлом фоне — 1.3:1. Проверяем, что светлая
    // палитра его НЕ унаследовала, а взяла собственный тёмно-зелёный.
    assert.notEqual(light['--t-acc'].toLowerCase(), dark['--t-acc'].toLowerCase());
    const phosphorOnLight = contrastRatio(dark['--t-acc'], light['--t-bg']);
    assert.ok(phosphorOnLight < AA_TEXT, 'контрольная точка: #33FF88 на светлом нечитаем');
});

test('светлая тема действительно светлее тёмной', () => {
    const lightBg = contrastRatio(light['--t-bg'], '#000000');
    const darkBg = contrastRatio(dark['--t-bg'], '#000000');
    assert.ok(lightBg > darkBg, 'фон светлой палитры обязан быть светлее фона тёмной');
});
