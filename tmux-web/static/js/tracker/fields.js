// tmux-web — плагин forge-tracker: конверсии значений полей карточки.
//
// ЗАЧЕМ ОТДЕЛЬНЫЙ ФАЙЛ. Карточка задачи (js/tracker/modals.js) импортирует
// `../core/dom.js`, а тот дёргает `document.getElementById` на уровне модуля —
// значит modals.js неимпортируем в голом node и НИЧТО из него не покрывается
// node-тестом. Конверсии же «строка модели ↔ значение нативного пикера» —
// это ровно то место, где ошибка стоит дороже всего: перепутанная зона или
// потерянная «голая дата» молча сдвигают срок на день. Поэтому они вынесены
// сюда, где нет ни DOM, ни сети, и покрыты tests/frontend/tracker_fields.test.mjs.
//
// ТРИ ФОРМЫ ОДНОЙ ДАТЫ, И ОНИ НЕ РАВНЫ:
//
//   1. модель (plugins/tracker/src/models.rs) — `YYYY-MM-DD` ИЛИ RFC 3339;
//      голая дата разрешена намеренно: Гантт рисует по дням, и выдумывать
//      полночь там не за чем (ровно это же правило у `parseDue` в parse.js);
//   2. `<input type="date">` — всегда `YYYY-MM-DD`, без зоны;
//   3. `<input type="datetime-local">` — `YYYY-MM-DDTHH:MM` в ЛОКАЛЬНОЙ зоне.
//
// ГЛАВНОЕ ПРАВИЛО ЭТОГО ФАЙЛА — КРУГОВОЙ ПРОГОН НИЧЕГО НЕ МЕНЯЕТ.
// Карточка шлёт в PATCH только те поля, которые человек ДЕЙСТВИТЕЛЬНО
// изменил (`buildPatch` сравнивает строку формы со строкой задачи). Если бы
// `to*` → `from*` возвращали не ту же строку, открытая и тут же сохранённая
// карточка переписывала бы срок каждой задаче. Отсюда два решения:
//
//   * голая дата в поле `due` показывается как `…T00:00` и уезжает обратно
//     голой датой — полночь в поле НЕ означает «полночь по UTC»;
//   * поле `start` вообще без времени: время планового начала не рисуется
//     нигде (см. `planSpan` в render-gantt.js — там только дни), а лишний
//     `T00:00` в модели пришлось бы либо хранить, либо сбрасывать при каждом
//     сохранении.
//
// ПРО ПРОСТАВЛЕННОЕ ВРЕМЯ В `start`. Если задача пришла из prompt-строки или
// из чужого клиента со временем (RFC 3339), поле показывает её ДЕНЬ, и при
// сохранении карточки время действительно теряется. Это осознанный размен:
// показать день и молча сохранить скрытое время невозможно — человек видел
// бы одно, а сравнение считало бы другое, и «неизменённое поле» перестало бы
// быть неизменённым.
//
// ОЦЕНКА — ТОЖЕ ПОЛЕ С ДВУМЯ ФОРМАМИ. Наружу всегда уходят МИНУТЫ
// (`estimate_min`), а выбирается либо типовая длительность, либо «своё».
// Значение не из списка (пришло из prompt-строки `~90м`, из br, из чужого
// клиента) обязано открыться КАК ЕСТЬ в режиме «своё»: молча подменить 90 на
// ближайший час — это тихая потеря пользовательских данных.

import { parseTs } from './render-list.js';

/** Голая дата модели: `YYYY-MM-DD`. */
const BARE_DATE = /^(\d{4})-(\d{2})-(\d{2})$/;

/** Значение `datetime-local` ровно в полночь (секунды браузер иногда пишет). */
const MIDNIGHT = /^(\d{4}-\d{2}-\d{2})T00:00(?::00(?:\.0+)?)?$/;

function p2(n) {
    return String(n).padStart(2, '0');
}

// ------------------------------------------------------- datetime-local ↔ RFC

/** Отметка `datetime-local` из RFC 3339 (в ЛОКАЛЬНОЙ зоне пользователя). */
export function toLocalInput(raw) {
    const d = parseTs(raw);
    if (!d) return '';
    return `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())}T${p2(d.getHours())}:${p2(d.getMinutes())}`;
}

/**
 * Значение `datetime-local` → RFC 3339 (UTC).
 *
 * `new Date("2026-09-08T14:02")` без смещения читается как ЛОКАЛЬНОЕ время
 * (в отличие от голой даты, которая читалась бы как UTC), поэтому ручной
 * разбор не нужен, а `toISOString` даёт ровно ту форму, которую понимает
 * `parse_ts` на сервере.
 */
export function fromLocalInput(value) {
    const s = String(value || '').trim();
    if (!s) return null;
    const d = new Date(s);
    return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

// ------------------------------------------------------------- date (без времени)

/**
 * Значение `<input type="date">` из строки модели.
 *
 * Голая дата отдаётся БЕЗ разбора в `Date` — так исключается сдвиг на сутки
 * в зонах западнее UTC (`new Date("2026-09-08")` — это полночь UTC, то есть
 * ещё 07.09 в Нью-Йорке).
 *
 * @param {string|null|undefined} raw
 * @returns {string} `YYYY-MM-DD` или `''`
 */
export function toDateInput(raw) {
    const s = String(raw == null ? '' : raw).trim();
    if (!s) return '';
    if (BARE_DATE.test(s)) return s;
    const d = parseTs(s);
    if (!d) return '';
    return `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())}`;
}

/**
 * Значение `<input type="date">` → строка модели (голая дата) или `null`.
 *
 * Пустое поле — это СБРОС (`null`), а не «оставить как было»: разницу между
 * «не передали» и «передали null» модель различает специально
 * (`Option<Option<String>>`), и в карточке она видна пользователю.
 *
 * @param {string} value
 * @returns {string|null}
 */
export function fromDateInput(value) {
    const s = String(value == null ? '' : value).trim();
    return BARE_DATE.test(s) ? s : null;
}

// ------------------------------------------------------------------ срок (due)

/**
 * Значение `<input type="datetime-local">` для поля `due`.
 *
 * У срока время суток осмысленно (`due:18:00` в prompt-строке, «сегодня
 * 18:00» в макете), поэтому пикер здесь с временем. Голая дата открывается
 * как полночь — иначе её нечем показать, — но обратно уезжает голой датой,
 * см. `fromDueInput`.
 */
export function toDueInput(raw) {
    const s = String(raw == null ? '' : raw).trim();
    if (!s) return '';
    if (BARE_DATE.test(s)) return `${s}T00:00`;
    return toLocalInput(s);
}

/**
 * Значение `datetime-local` поля `due` → строка модели или `null`.
 *
 * Полночь возвращается ГОЛОЙ ДАТОЙ. Это и сохраняет круговой прогон для
 * задач без времени (их большинство: так их создаёт `parseDue`), и совпадает
 * с тем, что вообще означает срок без времени — «до конца дня», а не «до
 * 00:00 по UTC». Срок, выставленный человеком ровно на полночь, тоже станет
 * голой датой: для Гантта и для `inDir` это один и тот же день.
 */
export function fromDueInput(value) {
    const s = String(value == null ? '' : value).trim();
    if (!s) return null;
    const m = MIDNIGHT.exec(s);
    if (m) return m[1];
    return fromLocalInput(s);
}

// ---------------------------------------------------------------- оценка

/**
 * Типовые длительности селектора оценки: минуты и подпись.
 * Шкала та же, что человек и так набирает в prompt-строке (`~2ч`).
 */
export const ESTIMATE_PRESETS = [
    [15, '15м'],
    [30, '30м'],
    [60, '1ч'],
    [120, '2ч'],
    [240, '4ч'],
    [480, '8ч'],
];

/** Значение селектора «своё». Не число — с минутами не пересекается. */
export const ESTIMATE_CUSTOM = 'custom';

/**
 * Как открыть селектор оценки для существующей задачи.
 *
 * @param {number|string|null|undefined} minutes `task.estimate_min`
 * @returns {{mode: string, custom: string}} `mode` — значение `<select>`
 *   (`''` — без оценки, минуты строкой, либо `'custom'`), `custom` — что
 *   положить в числовое поле «своё».
 */
export function estimateChoice(minutes) {
    const n = Number(minutes);
    if (minutes == null || minutes === '' || !Number.isFinite(n) || n <= 0) {
        return { mode: '', custom: '' };
    }
    const preset = ESTIMATE_PRESETS.some(([m]) => m === n);
    // Нестандартное значение открывается в «своём», а не подменяется
    // ближайшим типовым: 90 минут остаются 90 минутами.
    return preset ? { mode: String(n), custom: '' } : { mode: ESTIMATE_CUSTOM, custom: String(n) };
}

/**
 * Значение селектора оценки → то, что уйдёт в `buildPatch` (строка минут или
 * пустая строка = сброс).
 *
 * Пустое и нечисловое «своё» считается СБРОСОМ, а не «оставить как было»:
 * человек выбрал «своё» и стёр число — это осознанное «оценки нет».
 *
 * @param {string} mode значение `<select>`
 * @param {string} custom содержимое числового поля
 * @returns {string}
 */
export function estimateValue(mode, custom) {
    const sel = String(mode == null ? '' : mode).trim();
    if (!sel) return '';
    if (sel !== ESTIMATE_CUSTOM) return sel;
    const n = Number(String(custom == null ? '' : custom).trim());
    return Number.isFinite(n) && n > 0 ? String(Math.round(n)) : '';
}
