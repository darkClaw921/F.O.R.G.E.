// tmux-web — общий слой мессенджеров: математика цвета и режимы темы панели.
//
// ЧТО ЭТО ЗА КАТАЛОГ. `js/messenger/*` — узкий общий слой двух панелей
// (Telegram и VK). Правило приёма жёсткое: сюда попадает ТОЛЬКО код с НУЛЕВОЙ
// связью с мессенджером — не «почти нулевой». Всё, что знает про диалоги,
// сообщения, вложения или конкретный API, остаётся в `js/telegram/*` и
// `js/vk/*` двумя честными копиями.
//
// ПОЧЕМУ ИМЕННО ЭТОТ ФАЙЛ ПЕРВЫЙ. Математика цвета — эталонный случай общего
// кода: она не знает даже слова «сообщение», зато в ней уже зашит опыт
// (`oklch()` и `transparent` не разбираются ⇒ запасная тёмная тема). Вторая
// копия такого кода неизбежно отстанет от третьего нераспознанного формата —
// и разойдутся панели молча, без единого падения.
//
// ЧТО ЭТО НЕ ЗНАЧИТ. `js/telegram/format.js` НЕ редактируется: его копия этих
// функций остаётся на месте, панель Telegram продолжает пользоваться ею.
// Общий слой импортирует только VK. Миграция Telegram — отдельная задача
// после стабилизации VK; тогда телеграмный блок превратится в re-export, и
// все существующие тесты продолжат работать против тех же публичных имён.
//
// ПОЧЕМУ ВООБЩЕ НУЖНА ТЕМА ПАНЕЛИ. Обе панели нарисованы в терминальной
// палитре и НЕ наследуют токены приложения: их цвета объявлены на `#telegram`
// / `#vk` в собственных CSS. Значит переключение темы devforge их не касается
// — и это правильно, пока пользователь в тёмной теме. На светлой теме чёрная
// панель выглядит инородно, поэтому у вкладки есть свой тумблер ◐.
//
// КАК РАБОТАЕТ РЕЖИМ auto. Глобального флага «сейчас светлая тема» в devforge
// НЕТ: `js/themes/api.js::applyTheme` просто расставляет CSS-переменные на
// `:root`. Поэтому светлота выводится из вычисленного значения `--bg` —
// разбор цвета и яркость лежат здесь и покрыты Node-тестом
// (`tmux-web/tests/frontend/messenger_theme_core.test.mjs`).
//
// Модуль ЧИСТЫЙ: ни DOM, ни localStorage, ни импортов — грузится в Node
// как есть.

/** Порядок обхода тумблером ◐. */
export const THEME_MODES = ['auto', 'dark', 'light'];

/** Тема, которая применяется, когда режим `auto`, а цвет распознать не вышло. */
export const THEME_FALLBACK = 'dark';

/** Немногочисленные именованные цвета, которые реально встречаются в темах. */
const NAMED_COLORS = {
    black: { r: 0, g: 0, b: 0 },
    white: { r: 255, g: 255, b: 255 },
    silver: { r: 192, g: 192, b: 192 },
    gray: { r: 128, g: 128, b: 128 },
    grey: { r: 128, g: 128, b: 128 },
};

/**
 * Приводит режим к одному из [`THEME_MODES`].
 *
 * Любой мусор (в том числе значение из localStorage, записанное прошлой
 * версией) превращается в 'auto' — режим по умолчанию.
 *
 * @param {*} mode
 * @returns {'auto'|'dark'|'light'}
 */
export function normalizeThemeMode(mode) {
    const m = String(mode == null ? '' : mode).toLowerCase();
    return THEME_MODES.includes(m) ? m : 'auto';
}

/**
 * Следующий режим по кругу: auto → dark → light → auto.
 *
 * @param {*} mode
 * @returns {'auto'|'dark'|'light'}
 */
export function nextThemeMode(mode) {
    const idx = THEME_MODES.indexOf(normalizeThemeMode(mode));
    return THEME_MODES[(idx + 1) % THEME_MODES.length];
}

/**
 * Разбирает CSS-цвет в компоненты 0..255.
 *
 * Поддержаны формы, которые реально может отдать `getComputedStyle` или
 * пользовательская тема: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`,
 * `rgb(r,g,b)`, `rgba(r,g,b,a)`, современная запись `rgb(r g b / a)`,
 * проценты внутри `rgb()` и горстка именованных цветов.
 *
 * Всё остальное (`oklch()`, `color()`, `transparent`, градиент) — `null`:
 * вызывающий обязан иметь запасной вариант, а не гадать по мусору. Это не
 * теоретическая осторожность: редактор тем devforge позволяет ввести любую
 * строку, и `oklch()` в пользовательской теме встречается.
 *
 * @param {*} value
 * @returns {?{r: number, g: number, b: number}}
 */
export function parseCssColor(value) {
    const raw = String(value == null ? '' : value).trim().toLowerCase();
    if (!raw) return null;

    if (Object.prototype.hasOwnProperty.call(NAMED_COLORS, raw)) {
        return { ...NAMED_COLORS[raw] };
    }

    if (raw.charAt(0) === '#') {
        const hex = raw.slice(1);
        if (/^[0-9a-f]{3,4}$/.test(hex)) {
            return {
                r: parseInt(hex.charAt(0) + hex.charAt(0), 16),
                g: parseInt(hex.charAt(1) + hex.charAt(1), 16),
                b: parseInt(hex.charAt(2) + hex.charAt(2), 16),
            };
        }
        if (/^[0-9a-f]{6}$/.test(hex) || /^[0-9a-f]{8}$/.test(hex)) {
            return {
                r: parseInt(hex.slice(0, 2), 16),
                g: parseInt(hex.slice(2, 4), 16),
                b: parseInt(hex.slice(4, 6), 16),
            };
        }
        return null;
    }

    const m = /^rgba?\(([^)]*)\)$/.exec(raw);
    if (!m) return null;
    // И запятая, и пробел, и слэш альфы: `rgb(1,2,3)`, `rgb(1 2 3 / .5)`.
    const parts = m[1].split('/')[0].split(/[\s,]+/).filter((s) => s.length > 0);
    if (parts.length < 3) return null;
    const comp = parts.slice(0, 3).map((p) => {
        if (p.endsWith('%')) {
            const pct = Number.parseFloat(p.slice(0, -1));
            return Number.isFinite(pct) ? (pct * 255) / 100 : NaN;
        }
        return Number.parseFloat(p);
    });
    if (comp.some((n) => !Number.isFinite(n))) return null;
    const clamp = (n) => Math.min(255, Math.max(0, Math.round(n)));
    return { r: clamp(comp[0]), g: clamp(comp[1]), b: clamp(comp[2]) };
}

/**
 * Относительная яркость 0..1 по формуле 0.2126R + 0.7152G + 0.0722B на
 * НОРМИРОВАННЫХ (0..1) компонентах.
 *
 * Гамма-коррекция (sRGB → linear) намеренно НЕ применяется: нужен не
 * контрастный коэффициент по WCAG, а грубое «светлая тема или тёмная», и
 * простая формула даёт тот же ответ на всех штатных темах devforge, зато
 * читается без сносок.
 *
 * @param {?{r: number, g: number, b: number}} color
 * @returns {?number}
 */
export function relativeLuminance(color) {
    if (!color || typeof color !== 'object') return null;
    const { r, g, b } = color;
    if (![r, g, b].every((n) => Number.isFinite(n))) return null;
    return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
}

/**
 * Контрастное отношение двух цветов по WCAG 2.x: от 1:1 до 21:1.
 *
 * ОТДЕЛЬНАЯ функция, а не надстройка над [`relativeLuminance`], и это не
 * дублирование. У них разные задачи и потому разная математика:
 *
 * * [`relativeLuminance`] отвечает на вопрос «тема светлая или тёмная», и
 *   гамма-коррекция там намеренно опущена — на всех штатных темах devforge
 *   ответ тот же, зато формула читается без сносок;
 * * здесь нужен ПОРОГ ДОСТУПНОСТИ (4.5:1 для мелкого текста), а он определён
 *   ровно через гамма-скорректированную яркость sRGB. Без коррекции число
 *   расходится с чекерами в разы — на тёмных фонах особенно, то есть ровно
 *   там, где обе панели и живут.
 *
 * Нераспознанный цвет (`oklch()`, `transparent`, градиент) даёт `null`:
 * вызывающий обязан иметь запасной вариант, а не считать порог по мусору.
 *
 * @param {*} a — CSS-цвет либо `{r,g,b}`
 * @param {*} b — CSS-цвет либо `{r,g,b}`
 * @returns {?number} отношение (большее к меньшему), null ⇒ цвет не разобран
 */
export function contrastRatio(a, b) {
    const la = srgbLuminance(a);
    const lb = srgbLuminance(b);
    if (la == null || lb == null) return null;
    const hi = Math.max(la, lb);
    const lo = Math.min(la, lb);
    return (hi + 0.05) / (lo + 0.05);
}

/**
 * Гамма-скорректированная относительная яркость sRGB (WCAG 2.x), 0..1.
 *
 * Принимает и строку, и уже разобранные компоненты — вызывающему не нужно
 * знать, в каком виде у него цвет.
 *
 * @param {*} color
 * @returns {?number}
 */
function srgbLuminance(color) {
    const c = (color && typeof color === 'object' && 'r' in color)
        ? color
        : parseCssColor(color);
    if (!c) return null;
    const { r, g, b } = c;
    if (![r, g, b].every((n) => Number.isFinite(n))) return null;
    const lin = (v) => {
        const s = v / 255;
        return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
    };
    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

/**
 * Итоговая тема панели: что положить в `data-*-theme` её корневого элемента.
 *
 * В режимах 'dark'/'light' цвет приложения не смотрим вовсе — пользователь
 * зафиксировал вид панели явно. В 'auto' решает яркость `--bg` приложения:
 * L > 0.5 ⇒ светлая. Нераспознанный цвет ⇒ [`THEME_FALLBACK`].
 *
 * @param {*} mode
 * @param {*} appBg — значение CSS-переменной --bg
 * @returns {'dark'|'light'}
 */
export function resolveTheme(mode, appBg) {
    const m = normalizeThemeMode(mode);
    if (m !== 'auto') return m;
    const lum = relativeLuminance(parseCssColor(appBg));
    if (lum == null) return THEME_FALLBACK;
    return lum > 0.5 ? 'light' : 'dark';
}

/**
 * Подпись тумблера ◐: режим и — для 'auto' — что он сейчас выбрал.
 *
 * @param {*} mode
 * @param {*} resolved — 'dark' | 'light'
 * @returns {string}
 */
export function themeToggleTitle(mode, resolved) {
    const m = normalizeThemeMode(mode);
    const r = resolved === 'light' ? 'светлая' : 'тёмная';
    if (m === 'auto') return `Тема панели: авто (сейчас ${r}) — клик для тёмной`;
    if (m === 'dark') return 'Тема панели: тёмная — клик для светлой';
    return 'Тема панели: светлая — клик для авто';
}
