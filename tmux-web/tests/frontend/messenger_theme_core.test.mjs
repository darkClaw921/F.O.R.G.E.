// Регресс-тест общего слоя мессенджеров: math цвета и режимы темы панели.
//
// Грузит РЕАЛЬНЫЙ модуль tmux-web/static/js/messenger/theme-core.js через
// `await import()` — он не импортирует ни DOM, ни core/*, поэтому работает в
// Node как есть.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. `resolveTheme('auto', bg)` читает ПРОИЗВОЛЬНУЮ
// пользовательскую тему devforge: редактор тем позволяет ввести любую строку,
// включая `oklch()`, `color(display-p3 …)` и `transparent`. Нераспознанный
// цвет обязан давать ЗАПАСНУЮ ТЁМНУЮ тему, а не «светлую по умолчанию»:
// светлая палитра на тёмном фоне приложения — это белая вспышка во весь
// экран. Мутационная проверка: поменяйте THEME_FALLBACK на 'light' — тест
// обязан покраснеть (см. блок «fallback» ниже).
//
// Запуск: node tmux-web/tests/frontend/messenger_theme_core.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const modUrl = new URL('../../static/js/messenger/theme-core.js', import.meta.url).href;
const T = await import(modUrl);

let failures = 0;
let checks = 0;

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    }
}

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    }
}

// ---------- parseCssColor: hex ----------
eq(T.parseCssColor('#fff'), { r: 255, g: 255, b: 255 }, 'hex-3: белый');
eq(T.parseCssColor('#000'), { r: 0, g: 0, b: 0 }, 'hex-3: чёрный');
eq(T.parseCssColor('#0af'), { r: 0, g: 170, b: 255 }, 'hex-3: удвоение нибблов');
eq(T.parseCssColor('#0aff'), { r: 0, g: 170, b: 255 }, 'hex-4: альфа отбрасывается');
eq(T.parseCssColor('#050709'), { r: 5, g: 7, b: 9 }, 'hex-6: фон панели VK');
eq(T.parseCssColor('#050709FF'), { r: 5, g: 7, b: 9 }, 'hex-8: альфа отбрасывается');
eq(T.parseCssColor('  #FFF  '), { r: 255, g: 255, b: 255 }, 'hex: пробелы и регистр');
eq(T.parseCssColor('#ff'), null, 'hex-2: не цвет');
eq(T.parseCssColor('#fffff'), null, 'hex-5: не цвет');
eq(T.parseCssColor('#gggggg'), null, 'hex: не-hex символы');

// ---------- parseCssColor: rgb()/rgba() ----------
eq(T.parseCssColor('rgb(1,2,3)'), { r: 1, g: 2, b: 3 }, 'rgb: запятые');
eq(T.parseCssColor('rgb(1 2 3)'), { r: 1, g: 2, b: 3 }, 'rgb: пробелы (современная запись)');
eq(T.parseCssColor('rgba(1, 2, 3, 0.5)'), { r: 1, g: 2, b: 3 }, 'rgba: альфа не влияет');
eq(T.parseCssColor('rgb(1 2 3 / .5)'), { r: 1, g: 2, b: 3 }, 'rgb: альфа через слэш');
eq(T.parseCssColor('rgb(100%, 0%, 50%)'), { r: 255, g: 0, b: 128 }, 'rgb: проценты');
eq(T.parseCssColor('rgb(300, -20, 3)'), { r: 255, g: 0, b: 3 }, 'rgb: значения зажимаются в 0..255');
eq(T.parseCssColor('rgb(1,2)'), null, 'rgb: меньше трёх компонент');
eq(T.parseCssColor('rgb(a,b,c)'), null, 'rgb: нечисловые компоненты');

// ---------- parseCssColor: именованные ----------
eq(T.parseCssColor('black'), { r: 0, g: 0, b: 0 }, 'named: black');
eq(T.parseCssColor('WHITE'), { r: 255, g: 255, b: 255 }, 'named: регистр не важен');
eq(T.parseCssColor('grey'), T.parseCssColor('gray'), 'named: grey == gray');

// ---------- parseCssColor: то, ради чего нужен fallback ----------
eq(T.parseCssColor('oklch(0.7 0.1 250)'), null, 'oklch: не разбирается — это и есть повод для fallback');
eq(T.parseCssColor('color(display-p3 1 0 0)'), null, 'color(): не разбирается');
eq(T.parseCssColor('transparent'), null, 'transparent: не цвет фона');
eq(T.parseCssColor('linear-gradient(#000,#fff)'), null, 'градиент: не цвет');
eq(T.parseCssColor(''), null, 'пустая строка');
eq(T.parseCssColor(null), null, 'null');
eq(T.parseCssColor(undefined), null, 'undefined');
eq(T.parseCssColor({}), null, 'объект');
eq(T.parseCssColor('мусор'), null, 'произвольный мусор');

// ---------- relativeLuminance ----------
eq(T.relativeLuminance({ r: 0, g: 0, b: 0 }), 0, 'яркость: чёрный = 0');
ok(Math.abs(T.relativeLuminance({ r: 255, g: 255, b: 255 }) - 1) < 1e-9,
    'яркость: белый = 1 (с поправкой на float: сумма коэффициентов даёт 0.9999999999999999)');
ok(Math.abs(T.relativeLuminance({ r: 128, g: 128, b: 128 }) - 128 / 255) < 1e-9,
    'яркость: серый ≈ 0.5');
ok(T.relativeLuminance({ r: 0, g: 255, b: 0 }) > T.relativeLuminance({ r: 255, g: 0, b: 0 }),
    'яркость: зелёный весит больше красного (коэффициент 0.7152 против 0.2126)');
eq(T.relativeLuminance(null), null, 'яркость: null-цвет');
eq(T.relativeLuminance({ r: NaN, g: 0, b: 0 }), null, 'яркость: NaN-компонента');
eq(T.relativeLuminance('#fff'), null, 'яркость: строку не принимаем (нужен разобранный цвет)');

// ---------- normalizeThemeMode / nextThemeMode ----------
eq(T.THEME_MODES, ['auto', 'dark', 'light'], 'порядок режимов тумблера');
eq(T.normalizeThemeMode('dark'), 'dark', 'режим: как есть');
eq(T.normalizeThemeMode('LIGHT'), 'light', 'режим: регистр не важен');
eq(T.normalizeThemeMode('чушь'), 'auto', 'режим: мусор → auto');
eq(T.normalizeThemeMode(null), 'auto', 'режим: null → auto');
eq(T.normalizeThemeMode(undefined), 'auto', 'режим: undefined → auto (первый запуск)');

eq(T.nextThemeMode('auto'), 'dark', 'круг: auto → dark');
eq(T.nextThemeMode('dark'), 'light', 'круг: dark → light');
eq(T.nextThemeMode('light'), 'auto', 'круг: light → auto');
eq(T.nextThemeMode('мусор'), 'dark', 'круг: мусор считается auto');
// Три клика возвращают в исходную точку — иначе тумблер «застревает».
eq(T.nextThemeMode(T.nextThemeMode(T.nextThemeMode('auto'))), 'auto', 'круг замкнут за 3 клика');

// ---------- resolveTheme ----------
eq(T.resolveTheme('dark', '#ffffff'), 'dark', 'явный dark игнорирует светлый фон приложения');
eq(T.resolveTheme('light', '#000000'), 'light', 'явный light игнорирует тёмный фон приложения');
eq(T.resolveTheme('auto', '#ffffff'), 'light', 'auto: белый фон → светлая');
eq(T.resolveTheme('auto', '#050709'), 'dark', 'auto: фон панели VK → тёмная');
eq(T.resolveTheme('auto', 'rgb(250 250 250)'), 'light', 'auto: почти белый → светлая');
// Граница «светлая/тёмная» проходит по L > 0.5, то есть по компоненте 127/128:
// 127/255 = 0.498 (тёмная), 128/255 = 0.502 (светлая). Проверяем обе стороны —
// смена строгого `>` на `>=` не изменила бы ни одну из них, а вот сдвиг порога
// (например, на 0.6) покраснит вторую.
eq(T.resolveTheme('auto', '#7f7f7f'), 'dark', 'auto: на тёмной стороне середины (L≈0.498)');
eq(T.resolveTheme('auto', '#808080'), 'light', 'auto: на светлой стороне середины (L≈0.502)');

// ---------- fallback (мутационная точка) ----------
// Если THEME_FALLBACK сменить на 'light', покраснеют все четыре проверки
// ниже: нераспознанный фон обязан давать ТЁМНУЮ панель.
eq(T.THEME_FALLBACK, 'dark', 'запасная тема — тёмная');
eq(T.resolveTheme('auto', 'oklch(0.2 0.02 250)'), 'dark', 'auto: oklch не разобран → fallback');
eq(T.resolveTheme('auto', 'transparent'), 'dark', 'auto: transparent → fallback');
eq(T.resolveTheme('auto', ''), 'dark', 'auto: пустой --bg (стили ещё не применились) → fallback');
eq(T.resolveTheme('auto', null), 'dark', 'auto: null → fallback');

// ---------- themeToggleTitle ----------
ok(T.themeToggleTitle('auto', 'dark').includes('авто'), 'подпись: auto называет режим');
ok(T.themeToggleTitle('auto', 'light').includes('светлая'),
    'подпись: auto сообщает, что именно выбрано сейчас');
ok(T.themeToggleTitle('dark', 'dark').includes('тёмная'), 'подпись: dark');
ok(T.themeToggleTitle('light', 'light').includes('светлая'), 'подпись: light');
ok(T.themeToggleTitle('мусор', 'dark').includes('авто'), 'подпись: мусор считается auto');
// Подпись — это title кнопки, она обязана быть непустой в любом режиме.
for (const m of ['auto', 'dark', 'light', null, 'nonsense']) {
    ok(String(T.themeToggleTitle(m, 'dark') || '').length > 0, `подпись непуста для режима ${m}`);
}

console.log(`${checks - failures}/${checks} проверок прошло`);
if (failures > 0) console.error(`${failures} проверок упало`);
process.exit(failures > 0 ? 1 : 0);
