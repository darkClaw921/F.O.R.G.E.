// Регресс-тест ЧИТАЕМОСТИ палитры панели «Битрикс24» (css/bitrix.css).
//
// ============================================================================
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ
// ============================================================================
//
// У соседней панели (VK) был дефект, найденный только пользовательской
// приёмкой: «в тёмной теме не видно сообщение, на которое ответили». Токен
// приглушённого ТЕКСТА был объявлен цветом РАМОК, и текст давал 1.3:1 — то
// есть был физически невидим. Одним значением красились десять мест сразу.
//
// Здесь тест устроен так, чтобы поймать ту же ошибку ДО приёмки и в любой из
// двух тем: он перебирает ТОКЕНЫ, прочитанные из самого CSS, а не список
// цветов, скопированный в тест. Скопированные числа разошлись бы с файлом
// молча — ровно тем способом, каким и появляются такие дефекты.
//
// ============================================================================
// ПОЧЕМУ ЧИТАЕТСЯ САМ CSS
// ============================================================================
//
// Node-стенд без браузера каскад не считает, а подставлять числа руками
// означало бы проверять копию, а не файл. Отсюда: из `css/bitrix.css`
// вынимаются блоки объявлений токенов, а значения сверяются формулой WCAG из
// общего слоя (`js/messenger/theme-core.js::contrastRatio`) — третьей копии
// математики цвета в проекте не заводится.
//
// ============================================================================
// ЧТО ИМЕННО ПРОВЕРЯЕТСЯ
// ============================================================================
//
//   1. ПОЛНОТА: светлая тема переопределяет ВСЕ токены тёмной до единого.
//      Частичное переопределение даёт нечитаемые пятна — тёмный текст на
//      тёмном фоне в светлой теме.
//   2. КЛАССИФИКАЦИЯ: каждый объявленный токен отнесён к одной из трёх ролей
//      (фон / текст / рамки). Новый токен, забытый в этом файле, роняет тест —
//      иначе он молча остался бы непроверенным.
//   3. КОНТРАСТ: каждый ТЕКСТОВЫЙ токен против КАЖДОГО фона, на котором он
//      реально встречается, ≥ 4.5:1 (порог WCAG AA для мелкого текста; шрифт
//      панели 11-12px, послаблений нет).
//   4. РАЗДЕЛЕНИЕ РОЛЕЙ: приглушённый ТЕКСТ не равен цвету РАМОК — тот самый
//      дефект VK.
//   5. ИЕРАРХИЯ: приглушённый текст не ярче второстепенного, а тот не ярче
//      акцента. Иначе «приглушённое» перестаёт читаться как приглушённое.
//   6. РАЗЛИЧИМОСТЬ ПАНЕЛЕЙ: акцент обязан отличаться и от зелёного Telegram,
//      и от синего VK. Решение ФУНКЦИОНАЛЬНОЕ, а не эстетическое: три
//      одинаково выглядящие панели — это сообщение, ушедшее не в тот чат.
//
// Запуск: node tmux-web/tests/frontend/bitrix_theme_contrast.test.mjs

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

/** Порог WCAG для мелкого текста. Шрифт панели 11-12px — послаблений нет. */
const AA_TEXT = 4.5;

const CSS_PATH = new URL('../../static/css/bitrix.css', import.meta.url);
const css = readFileSync(CSS_PATH, 'utf8');

/**
 * Объявления токенов из одного блока правил.
 *
 * Селектор ищется как есть, тело берётся до первой закрывающей скобки:
 * вложенных блоков внутри объявления палитры не бывает, а полноценный парсер
 * CSS здесь был бы отдельной библиотекой с собственными багами. Хвостовой
 * комментарий с цифрой контраста отрезается — он документация, не значение.
 *
 * @param {string} selector
 * @returns {Object<string, string>} `--b-bg` → `#08070A`
 */
function tokensOf(selector) {
    const at = css.indexOf(`${selector} {`);
    if (at < 0) throw new Error(`блок «${selector}» не найден в css/bitrix.css`);
    const body = css.slice(at, css.indexOf('\n}', at));
    const out = {};
    for (const m of body.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) {
        out[m[1]] = m[2].split('/*')[0].trim();
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

const dark = tokensOf('#bitrix');
const light = tokensOf('#bitrix[data-b24-theme="light"]');

// ---------------------------------------------------------------------------
// Роли токенов
//
// ФОНЫ. `--b-active` — не «ещё один фон», а подсветка активной строки списка и
// hover кнопок: текст на ней лежит тот же самый, поэтому она обязана
// проверяться наравне с остальными.
//
// ТЕКСТ. Для каждого токена перечислены ФОНЫ, НА КОТОРЫХ ОН РЕАЛЬНО ЛЕЖИТ.
// Список не «все на всех»: `--b-green` красит ровно одну вещь — метку
// `.b24-mode-connected` в статусбаре (`--b-bar`), и требовать от него
// читаемости на подсветке активной строки значило бы придумывать сочетание,
// которого в файле нет, и подгонять под него палитру.
//
// РАМКИ. `--b-acc-faint` — цвет ГРАНИЦ. Текстом он не бывает никогда, и
// именно попытка покрасить им текст была дефектом VK.
// ---------------------------------------------------------------------------

const BACKDROPS = ['--b-bg', '--b-panel', '--b-bar', '--b-active'];

const TEXT_ON = {
    '--b-acc': BACKDROPS,
    '--b-acc-dim': BACKDROPS,
    '--b-acc-hi': BACKDROPS,
    '--b-faint-text': BACKDROPS,
    '--b-red': BACKDROPS,
    '--b-cyan': BACKDROPS,
    '--b-violet': BACKDROPS,
    // Только статусбар: `.b24-mode-connected`. Плюс `--b-bg` — статусбар мог
    // бы наследовать фон панели, и проверить обе поверхности дешевле, чем
    // объяснять, почему проверена одна.
    '--b-green': ['--b-bg', '--b-bar'],
};

const FRAME_ONLY = ['--b-acc-faint'];

// ---------------------------------------------------------------------------
// 1. Полнота светлой палитры
// ---------------------------------------------------------------------------

{
    const darkNames = Object.keys(dark).sort();
    const lightNames = Object.keys(light).sort();
    ok(darkNames.length > 0, 'тёмная палитра объявлена на #bitrix');
    eq(lightNames, darkNames,
        'светлая тема переопределяет ВСЕ токены тёмной до единого'
        + ' (частичное переопределение даёт нечитаемые пятна)');
    for (const name of darkNames) {
        ok(resolve(dark, name) !== resolve(light, name),
            `${name}: светлое значение отличается от тёмного (иначе тема не сменилась)`);
    }
}

// ---------------------------------------------------------------------------
// 2. Классификация: непроверенных токенов не бывает
// ---------------------------------------------------------------------------

{
    const classified = new Set([...BACKDROPS, ...Object.keys(TEXT_ON), ...FRAME_ONLY]);
    const declared = Object.keys(dark);
    const unknown = declared.filter((n) => !classified.has(n));
    eq(unknown, [],
        'каждый объявленный токен отнесён к роли (фон/текст/рамки);'
        + ' новый токен обязан быть добавлен в этот тест, иначе он не проверяется');
    const missing = Array.from(classified).filter((n) => !declared.includes(n));
    eq(missing, [], 'в тесте нет ролей для токенов, которых в CSS уже нет');
}

// ---------------------------------------------------------------------------
// 3. Контраст: каждый текстовый токен на каждом своём фоне
// ---------------------------------------------------------------------------

for (const [themeName, tokens] of [['тёмная', dark], ['светлая', light]]) {
    for (const [token, backdrops] of Object.entries(TEXT_ON)) {
        const color = resolve(tokens, token);
        ok(parseCssColor(color) !== null,
            `${themeName} тема: ${token} разбирается как цвет (${color})`);
        for (const bg of backdrops) {
            const ratio = contrastRatio(color, resolve(tokens, bg));
            ok(ratio !== null && ratio >= AA_TEXT,
                `${themeName} тема: ${token} (${color}) против ${bg}`
                + ` (${resolve(tokens, bg)}) = ${ratio === null ? 'не разобрано' : ratio.toFixed(2)}:1,`
                + ` нужно ≥ ${AA_TEXT}:1`);
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Приглушённый ТЕКСТ — не цвет РАМОК (дефект, найденный в VK)
// ---------------------------------------------------------------------------

for (const [themeName, tokens] of [['тёмная', dark], ['светлая', light]]) {
    const faintRaw = tokens['--b-faint-text'];
    ok(faintRaw !== undefined, `${themeName} тема: токен --b-faint-text объявлен`);
    ok(!/^var\(\s*--b-acc-faint\s*\)$/.test(String(faintRaw)),
        `${themeName} тема: --b-faint-text не должен быть псевдонимом рамочного`
        + ` --b-acc-faint (сейчас «${faintRaw}»)`);
    ok(resolve(tokens, '--b-faint-text') !== resolve(tokens, '--b-acc-faint'),
        `${themeName} тема: значение --b-faint-text не совпадает с --b-acc-faint`);
    // И рамочный цвет обязан ОСТАВАТЬСЯ рамочным: текстом он не годится, и
    // проверка снизу не даёт «починить» его подъёмом контраста до текстового —
    // тогда рамки стали бы линиями поверх интерфейса.
    const frame = contrastRatio(resolve(tokens, '--b-acc-faint'), resolve(tokens, '--b-bg'));
    ok(frame !== null && frame < 3,
        `${themeName} тема: --b-acc-faint остаётся рамочным (${frame.toFixed(2)}:1 < 3:1)`);
}

// ---------------------------------------------------------------------------
// 5. Иерархия приглушённости сохранена
// ---------------------------------------------------------------------------

for (const [themeName, tokens] of [['тёмная', dark], ['светлая', light]]) {
    const bg = resolve(tokens, '--b-bg');
    const hi = contrastRatio(resolve(tokens, '--b-acc-hi'), bg);
    const acc = contrastRatio(resolve(tokens, '--b-acc'), bg);
    const dim = contrastRatio(resolve(tokens, '--b-acc-dim'), bg);
    const faint = contrastRatio(resolve(tokens, '--b-faint-text'), bg);
    // Основной текст сообщений — самый заметный: он и есть содержание панели.
    ok(hi > acc, `${themeName}: основной текст (${hi.toFixed(2)}) заметнее акцента (${acc.toFixed(2)})`);
    // Приглушённый текст не имеет права обогнать второстепенный: иначе метка
    // времени кричала бы громче имени собеседника.
    ok(faint <= dim + 1e-9,
        `${themeName}: приглушённый текст (${faint.toFixed(2)}) не ярче --b-acc-dim (${dim.toFixed(2)})`);
}

// ---------------------------------------------------------------------------
// 6. Панели различимы с первого взгляда
// ---------------------------------------------------------------------------

{
    // Акценты соседей читаются из ИХ файлов, а не копируются числами: смена
    // палитры VK или Telegram обязана проверяться против нашей, а не тихо
    // разъезжаться с константой в тесте.
    const vkCss = readFileSync(new URL('../../static/css/vk.css', import.meta.url), 'utf8');
    const tgCss = readFileSync(new URL('../../static/css/telegram.css', import.meta.url), 'utf8');
    const vkAcc = /--v-acc:\s*([^;/]+)/.exec(vkCss);
    const tgAcc = /--t-grn:\s*([^;/]+)/.exec(tgCss);
    ok(vkAcc !== null, 'акцент VK найден в css/vk.css');
    ok(tgAcc !== null, 'акцент Telegram найден в css/telegram.css');

    const b24 = resolve(dark, '--b-acc');
    const vk = vkAcc[1].trim();
    const tg = tgAcc[1].trim();
    ok(b24.toLowerCase() !== vk.toLowerCase(), `акцент Битрикс24 (${b24}) отличается от VK (${vk})`);
    ok(b24.toLowerCase() !== tg.toLowerCase(), `акцент Битрикс24 (${b24}) отличается от Telegram (${tg})`);

    // Различие не только в значении, но и в ОТТЕНКЕ: три акцента обязаны
    // лежать в разных секторах цветового круга, иначе «оранжевый» от
    // «зелёного» на глаз в терминальной палитре не отличить.
    const hue = (hex) => {
        const c = parseCssColor(hex);
        const [r, g, b] = [c.r / 255, c.g / 255, c.b / 255];
        const max = Math.max(r, g, b);
        const min = Math.min(r, g, b);
        const d = max - min;
        if (d === 0) return 0;
        let h;
        if (max === r) h = ((g - b) / d) % 6;
        else if (max === g) h = (b - r) / d + 2;
        else h = (r - g) / d + 4;
        return ((h * 60) + 360) % 360;
    };
    const dist = (a, b) => {
        const raw = Math.abs(a - b) % 360;
        return raw > 180 ? 360 - raw : raw;
    };
    const hB24 = hue(b24);
    const hVk = hue(vk);
    const hTg = hue(tg);
    ok(dist(hB24, hVk) >= 60,
        `оттенок Битрикс24 (${hB24.toFixed(0)}°) далёк от VK (${hVk.toFixed(0)}°): ${dist(hB24, hVk).toFixed(0)}° ≥ 60°`);
    ok(dist(hB24, hTg) >= 60,
        `оттенок Битрикс24 (${hB24.toFixed(0)}°) далёк от Telegram (${hTg.toFixed(0)}°): ${dist(hB24, hTg).toFixed(0)}° ≥ 60°`);
}

// ---------------------------------------------------------------------------
// 7. Сама математика: контрольные точки формулы WCAG
// ---------------------------------------------------------------------------

{
    eq(Math.round(contrastRatio('#FFFFFF', '#000000')), 21, 'белое к чёрному — 21:1');
    eq(Math.round(contrastRatio('#000000', '#000000')), 1, 'цвет сам с собой — 1:1');
    eq(contrastRatio('oklch(0.5 0.1 200)', '#000000'), null,
        'нераспознанный цвет даёт null, а не число из мусора');
    // Порядок аргументов не меняет ответа: отношение считается большим к меньшему.
    eq(
        contrastRatio('#FF9F43', '#08070A').toFixed(4),
        contrastRatio('#08070A', '#FF9F43').toFixed(4),
        'отношение симметрично',
    );
}

console.log(`bitrix_theme_contrast: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
