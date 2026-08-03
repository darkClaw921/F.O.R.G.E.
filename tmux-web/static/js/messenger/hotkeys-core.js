// tmux-web / общий слой мессенджеров — движок клавиатурных комбинаций.
//
// Модуль НАМЕРЕННО чистый и НАМЕРЕННО не знает ни одного мессенджера: ни DOM,
// ни сети, ни реестра действий. Всё, что здесь есть, — функции над строками и
// объектами события; реестр действий передаётся ПЕРВЫМ аргументом (`actions`)
// и живёт в панели (`js/vk/keys.js`, `js/telegram/keys.js`).
//
// ── Почему движок вынесен в общий слой, а рисующие модули — нет
//
// Привязка клавиш — это ПОЛЬЗОВАТЕЛЬСКАЯ НАСТРОЙКА, сохранённая в файле
// (`user_settings.json`, поля `*_hotkeys`). Баг разбора `event.code`,
// исправленный в одной копии и не исправленный в другой, даёт жалобу вида «у
// меня в Telegram Ctrl+Shift+X работает, а в VK нет» — её невозможно
// диагностировать по симптому, потому что снаружи обе панели выглядят
// одинаково. Остальной код панели такого свойства не имеет: расхождение в
// рендере видно сразу и чинится там, где видно.
//
// ── Отношение к `js/telegram/keys.js`
//
// Telegram на это ядро НЕ мигрирован: панель работает, и её правка ради
// красоты графа импортов — чистый риск без пользовательской выгоды. Вместо
// миграции дрейф ловит СТРАЖ — `tests/frontend/messenger_shared_parity.test.mjs`
// импортирует ОБЕ реализации и требует совпадения результатов на таблице
// входов. Правка одной копии без второй = красный тест, а не молчаливое
// расхождение. Миграция (`telegram/keys.js` → re-export-шим) — отдельная
// задача после стабилизации VK.
//
// ── Почему сопоставление идёт по `event.code`, а не по `event.key`
//
// Панели мессенджеров — место, где по-русски печатают чаще, чем по-английски.
// При русской раскладке `event.key` для физической клавиши J вернёт 'о', и
// хоткей 'j' перестал бы работать ровно у тех пользователей, ради которых
// фича делается. `event.code` описывает физическую клавишу и от раскладки не
// зависит, поэтому канонический вид комбинации строится из него, а `event.key`
// остаётся лишь фолбэком для клавиш без кода (экранные клавиатуры).
//
// ── Канонический вид комбинации
//
// `[Ctrl+][Alt+][Shift+][Meta+]<клавиша>` — модификаторы всегда в этом порядке,
// буквы в нижнем регистре: 'j', 'Shift+g', 'Ctrl+Enter', 'Shift+/'. Порядок
// фиксирован, чтобы строка годилась ключом в Map: 'Shift+Ctrl+x' и
// 'Ctrl+Shift+x' обязаны быть одной записью, иначе конфликт двух действий
// остался бы незамеченным.
//
// ── Форма реестра действий (контракт аргумента `actions`)
//
// Массив объектов `{id, title, group, combo, hint?, fixed?}`:
//   id     — стабильный ключ настройки (`[a-z0-9_.-]`, см. sanitize_hotkeys);
//   title  — подпись в шпаргалке и настройках;
//   group  — имя группы (только для группировки в UI);
//   combo  — дефолтная комбинация в любом виде, нормализуется здесь;
//   hint   — пояснение под строкой (необязательно);
//   fixed  — «не переназначается» (Escape: выход из режима обязан быть один).

/** Модификаторы в каноническом порядке. */
const MODIFIER_ORDER = ['Ctrl', 'Alt', 'Shift', 'Meta'];

/** Клавиши-модификаторы сами по себе комбинацией не являются. */
const MODIFIER_KEYS = new Set(['Control', 'Alt', 'Shift', 'Meta', 'OS', 'AltGraph', 'CapsLock']);

/**
 * `event.code` → каноническая клавиша.
 *
 * Буквы и цифры разворачиваются формулой, здесь только то, что формулой не
 * берётся. Пунктуация перечислена по US-раскладке: код физической клавиши от
 * раскладки не зависит, а подпись в шпаргалке нужна одна и та же.
 */
const CODE_MAP = {
    BracketLeft: '[',
    BracketRight: ']',
    Slash: '/',
    Backslash: '\\',
    Semicolon: ';',
    Quote: "'",
    Comma: ',',
    Period: '.',
    Minus: '-',
    Equal: '=',
    Backquote: '`',
    Space: 'Space',
    Enter: 'Enter',
    NumpadEnter: 'Enter',
    Escape: 'Escape',
    Tab: 'Tab',
    Backspace: 'Backspace',
    Delete: 'Delete',
    Home: 'Home',
    End: 'End',
    PageUp: 'PageUp',
    PageDown: 'PageDown',
    ArrowUp: 'ArrowUp',
    ArrowDown: 'ArrowDown',
    ArrowLeft: 'ArrowLeft',
    ArrowRight: 'ArrowRight',
};

/** Как выглядит символ с зажатым Shift (US) — только для подписи в UI. */
const SHIFT_GLYPH = {
    '/': '?',
    '1': '!',
    '2': '@',
    '3': '#',
    '4': '$',
    '5': '%',
    '6': '^',
    '7': '&',
    '8': '*',
    '9': '(',
    '0': ')',
    '-': '_',
    '=': '+',
    '[': '{',
    ']': '}',
    ';': ':',
    "'": '"',
    ',': '<',
    '.': '>',
    '\\': '|',
    '`': '~',
};

/** Подписи спец-клавиш в шпаргалке. */
const KEY_GLYPH = {
    Enter: '⏎',
    Escape: 'Esc',
    Space: 'Пробел',
    Tab: 'Tab',
    ArrowUp: '↑',
    ArrowDown: '↓',
    ArrowLeft: '←',
    ArrowRight: '→',
    PageUp: 'PgUp',
    PageDown: 'PgDn',
    Backspace: '⌫',
    Delete: 'Del',
};

/** Подписи модификаторов. */
const MOD_GLYPH = { Ctrl: 'Ctrl', Alt: '⌥', Shift: '⇧', Meta: '⌘' };

/**
 * Каноническая клавиша из `event.code` (или `event.key` как фолбэк).
 *
 * @param {?{code?: string, key?: string}} e
 * @returns {?string} null — если клавишу опознать нельзя или это модификатор
 */
export function keyFromEvent(e) {
    if (!e) return null;
    const code = typeof e.code === 'string' ? e.code : '';
    if (code) {
        if (/^Key[A-Z]$/.test(code)) return code.slice(3).toLowerCase();
        if (/^Digit[0-9]$/.test(code)) return code.slice(5);
        if (/^Numpad[0-9]$/.test(code)) return code.slice(6);
        if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
        if (Object.prototype.hasOwnProperty.call(CODE_MAP, code)) return CODE_MAP[code];
    }
    // Фолбэк для окружений без code (экранные клавиатуры, синтетические
    // события в тестах): берём key, но только там, где он однозначен.
    const key = typeof e.key === 'string' ? e.key : '';
    if (!key) return null;
    if (MODIFIER_KEYS.has(key)) return null;
    if (key === ' ') return 'Space';
    if (key.length === 1) {
        const lower = key.toLowerCase();
        // Символ, набранный с Shift ('?', '!'), приводим к базовой клавише —
        // Shift и так поедет отдельным модификатором.
        for (const base of Object.keys(SHIFT_GLYPH)) {
            if (SHIFT_GLYPH[base] === key) return base;
        }
        return lower;
    }
    if (Object.prototype.hasOwnProperty.call(CODE_MAP, key)) return CODE_MAP[key];
    return key;
}

/**
 * Каноническая комбинация из события клавиатуры.
 *
 * @param {?object} e — KeyboardEvent или его подобие
 * @returns {?string} null — если нажат один модификатор или клавиша неизвестна
 */
export function comboFromEvent(e) {
    if (!e) return null;
    if (typeof e.key === 'string' && MODIFIER_KEYS.has(e.key)) return null;
    const key = keyFromEvent(e);
    if (!key) return null;
    const mods = [];
    if (e.ctrlKey) mods.push('Ctrl');
    if (e.altKey) mods.push('Alt');
    if (e.shiftKey) mods.push('Shift');
    if (e.metaKey) mods.push('Meta');
    return mods.length ? `${mods.join('+')}+${key}` : key;
}

/**
 * Приводит записанную строку к каноническому виду.
 *
 * Принимает пользовательский порядок модификаторов и синонимы (Cmd/Command →
 * Meta, Control → Ctrl, Option → Alt), потому что комбинация может приехать из
 * настроек, написанных руками в JSON.
 *
 * @param {?string} raw
 * @returns {?string} null — если строка пустая, из одних модификаторов или
 *   содержит неизвестный модификатор
 */
export function normalizeCombo(raw) {
    if (typeof raw !== 'string') return null;
    // Сам '+' как клавиша не встречается: физическая клавиша '=' приезжает из
    // события кодом Equal, а '+' на ней — это уже Shift+=. Поэтому строка из
    // одних плюсов — просто мусор.
    const parts = raw.split('+').map((p) => p.trim()).filter((p) => p !== '');
    if (parts.length === 0) return null;
    const mods = new Set();
    let key = null;
    for (const part of parts) {
        const canonMod = modifierName(part);
        if (canonMod) {
            mods.add(canonMod);
            continue;
        }
        // Вторая не-модификаторная часть означает мусор вида 'a+b'.
        if (key !== null) return null;
        key = canonKey(part);
        if (key === null) return null;
    }
    if (key === null) return null;
    const ordered = MODIFIER_ORDER.filter((m) => mods.has(m));
    return ordered.length ? `${ordered.join('+')}+${key}` : key;
}

/** Каноническое имя модификатора или null. */
export function modifierName(part) {
    const low = String(part).toLowerCase();
    if (low === 'ctrl' || low === 'control') return 'Ctrl';
    if (low === 'alt' || low === 'option' || low === 'opt') return 'Alt';
    if (low === 'shift') return 'Shift';
    if (low === 'meta' || low === 'cmd' || low === 'command' || low === 'super' || low === 'win') {
        return 'Meta';
    }
    return null;
}

/** Каноническое имя клавиши или null. */
export function canonKey(part) {
    const raw = String(part);
    if (raw.length === 1) {
        // Символ с Shift ('?') записываем базовой клавишей: иначе одна и та же
        // физическая комбинация имела бы два разных ключа в Map.
        for (const base of Object.keys(SHIFT_GLYPH)) {
            if (SHIFT_GLYPH[base] === raw) return base;
        }
        return raw.toLowerCase();
    }
    const known = ['Enter', 'Escape', 'Space', 'Tab', 'Backspace', 'Delete', 'Home', 'End',
        'PageUp', 'PageDown', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'];
    const hit = known.find((k) => k.toLowerCase() === raw.toLowerCase());
    if (hit) return hit;
    if (/^f([1-9]|1[0-9]|2[0-4])$/i.test(raw)) return raw.toUpperCase();
    return null;
}

/**
 * Человекочитаемая подпись комбинации: 'Shift+/' → '⇧?', 'Enter' → '⏎'.
 *
 * @param {?string} combo
 * @returns {string} пустая строка для мусора
 */
export function formatCombo(combo) {
    const canon = normalizeCombo(combo);
    if (!canon) return '';
    const parts = canon.split('+');
    const key = parts[parts.length - 1];
    const mods = parts.slice(0, -1);
    const hasShift = mods.includes('Shift');
    let label;
    if (Object.prototype.hasOwnProperty.call(KEY_GLYPH, key)) {
        label = KEY_GLYPH[key];
    } else if (hasShift && Object.prototype.hasOwnProperty.call(SHIFT_GLYPH, key)) {
        // '?' понятнее, чем '⇧/', но сам ⇧ оставляем: он показывает, что
        // клавишу надо нажимать с шифтом на любой раскладке.
        label = SHIFT_GLYPH[key];
    } else if (key.length === 1) {
        label = key.toUpperCase();
    } else {
        label = key;
    }
    const modLabels = mods.map((m) => MOD_GLYPH[m] || m);
    return modLabels.length ? `${modLabels.join('')}${label}` : label;
}

/**
 * Действие по id или null.
 *
 * @param {Array<object>} actions — реестр панели
 * @param {*} id
 */
export function findAction(actions, id) {
    const list = Array.isArray(actions) ? actions : [];
    return list.find((a) => a && a.id === String(id)) || null;
}

/**
 * Карта дефолтных привязок: id → комбинация.
 *
 * @param {Array<object>} actions
 * @returns {Map<string, string>}
 */
export function defaultBindings(actions) {
    const map = new Map();
    for (const a of (Array.isArray(actions) ? actions : [])) map.set(a.id, a.combo);
    return map;
}

/**
 * Отбрасывает из карты переопределений всё, что нельзя применить.
 *
 * Зеркало backend-санитайзера (`user_settings.rs::sanitize_hotkeys`), но
 * строже: здесь известен список действий, поэтому чужие id тоже отсеиваются.
 * Настройки при этом НЕ портятся — фильтрация только на чтение, чтобы запись
 * от новой версии клиента пережила откат на старую.
 *
 * @param {?object} raw — сырая карта из настроек
 * @param {Array<object>} actions — реестр панели
 * @returns {Map<string, string>} id → каноническая комбинация
 */
export function sanitizeOverrides(raw, actions) {
    const out = new Map();
    if (!raw || typeof raw !== 'object') return out;
    const entries = raw instanceof Map ? Array.from(raw.entries()) : Object.entries(raw);
    for (const [id, combo] of entries) {
        const action = findAction(actions, id);
        if (!action) continue;
        // Фиксированные действия переназначать нельзя — даже если запись
        // как-то оказалась в файле.
        if (action.fixed) continue;
        const canon = normalizeCombo(combo);
        if (!canon) continue;
        out.set(action.id, canon);
    }
    return out;
}

/**
 * Итоговые привязки с учётом переопределений.
 *
 * Конфликт разрешается по правилу «переопределение сильнее дефолта, а среди
 * переопределений — то, что раньше в реестре»: иначе одна комбинация активировала
 * бы два действия и поведение зависело бы от порядка перебора. Проигравшее
 * действие остаётся БЕЗ клавиши (combo: null) и честно показывается таким в
 * настройках — молча вернуть ему дефолт значило бы соврать пользователю,
 * который этот дефолт только что занял.
 *
 * @param {Array<object>} actions — реестр панели
 * @param {?object} overrides — сырая карта переопределений
 * @returns {{bindings: Map<string,string>, byCombo: Map<string,string>,
 *   conflicts: Array<{combo: string, winner: string, loser: string}>,
 *   unbound: Array<string>}}
 */
export function resolveBindings(actions, overrides) {
    const list = Array.isArray(actions) ? actions : [];
    const custom = sanitizeOverrides(overrides, list);
    const bindings = new Map();
    const byCombo = new Map();
    const conflicts = [];
    const unbound = [];

    // Два прохода: сначала все переопределения (они сильнее), затем дефолты
    // тех действий, что остались нетронутыми. Один проход по реестру дал бы
    // дефолту раннего действия победу над переопределением позднего.
    for (const action of list) {
        if (!custom.has(action.id)) continue;
        claim(action.id, custom.get(action.id));
    }
    for (const action of list) {
        if (custom.has(action.id)) continue;
        claim(action.id, action.combo);
    }

    function claim(id, combo) {
        const canon = normalizeCombo(combo);
        if (!canon) {
            unbound.push(id);
            return;
        }
        const owner = byCombo.get(canon);
        if (owner) {
            conflicts.push({ combo: canon, winner: owner, loser: id });
            unbound.push(id);
            return;
        }
        byCombo.set(canon, id);
        bindings.set(id, canon);
    }

    return { bindings, byCombo, conflicts, unbound };
}

/**
 * Строки для таблицы настроек и шпаргалки.
 *
 * @param {Array<object>} actions
 * @param {?object} overrides
 * @returns {Array<{id, title, group, hint, combo, label, isDefault, fixed, conflictWith}>}
 */
export function bindingRows(actions, overrides) {
    const list = Array.isArray(actions) ? actions : [];
    const { bindings, conflicts } = resolveBindings(list, overrides);
    const lostTo = new Map();
    for (const c of conflicts) lostTo.set(c.loser, c.winner);
    return list.map((a) => {
        const combo = bindings.get(a.id) || null;
        return {
            id: a.id,
            title: a.title,
            group: a.group,
            hint: a.hint || '',
            combo,
            label: combo ? formatCombo(combo) : '',
            isDefault: combo === normalizeCombo(a.combo),
            fixed: !!a.fixed,
            conflictWith: lostTo.get(a.id) || null,
        };
    });
}

/**
 * Карта переопределений для отправки на backend: только отличия от дефолта.
 *
 * Хранить полную карту было бы вредно: смена дефолта в новой версии не
 * доехала бы до пользователей, у которых «всё как из коробки».
 *
 * @param {Array<object>} actions
 * @param {?Map<string,string>|object} bindings — id → комбинация
 * @returns {object} plain-объект (JSON-совместимый)
 */
export function overridesFromBindings(actions, bindings) {
    const src = bindings instanceof Map
        ? bindings
        : new Map(Object.entries(bindings || {}));
    const out = {};
    for (const action of (Array.isArray(actions) ? actions : [])) {
        if (action.fixed) continue;
        if (!src.has(action.id)) continue;
        const canon = normalizeCombo(src.get(action.id));
        if (!canon) continue;
        if (canon === normalizeCombo(action.combo)) continue;
        out[action.id] = canon;
    }
    return out;
}

/**
 * Поле ввода ли под курсором.
 *
 * Список повторяет `hotkeys.js::isEditingTarget` — специально не импортируется:
 * тот файл классический скрипт (не модуль), и заимствовать его нечем.
 */
export function isEditableTarget(el) {
    if (!el) return false;
    const tag = String(el.tagName || '').toLowerCase();
    if (tag === 'input' || tag === 'textarea' || tag === 'select') return true;
    if (el.isContentEditable) return true;
    if (el.classList && typeof el.classList.contains === 'function'
        && el.classList.contains('xterm-helper-textarea')) {
        return true;
    }
    return false;
}

/**
 * Можно ли применить комбинацию в текущем контексте.
 *
 * Внутри поля ввода одиночные клавиши обязаны доходить до поля — иначе 'j' не
 * набрать. Пропускаем только комбинации с Ctrl/Alt/Meta и фиксированный Escape.
 *
 * @param {string} combo — канонический вид
 * @param {{editing: boolean}} ctx
 */
export function comboAllowedIn(combo, ctx) {
    const canon = normalizeCombo(combo);
    if (!canon) return false;
    if (!ctx || !ctx.editing) return true;
    if (canon === 'Escape') return true;
    // Shift сюда не входит намеренно: 'Shift+g' — это заглавная G, обычный
    // символ при наборе текста.
    const mods = canon.split('+').slice(0, -1);
    return mods.includes('Ctrl') || mods.includes('Alt') || mods.includes('Meta');
}

/**
 * Действие для события — или null.
 *
 * @param {?object} e — KeyboardEvent
 * @param {{byCombo: Map<string,string>}} resolved — результат resolveBindings
 * @param {{editing?: boolean}} [ctx]
 * @returns {?string} id действия
 */
export function actionForEvent(e, resolved, ctx) {
    if (!e || !resolved || !resolved.byCombo) return null;
    const combo = comboFromEvent(e);
    if (!combo) return null;
    const id = resolved.byCombo.get(combo);
    if (!id) return null;
    const editing = !!(ctx && ctx.editing);
    if (!comboAllowedIn(combo, { editing })) return null;
    return id;
}

/**
 * Группы со строками — для шпаргалки и настроек.
 *
 * @param {Array<object>} actions
 * @param {Array<string>} groups — порядок групп
 * @param {?object} overrides
 */
export function groupedRows(actions, groups, overrides) {
    const order = Array.isArray(groups) ? groups : [];
    const rows = bindingRows(actions, overrides);
    const out = [];
    for (const name of order) {
        const items = rows.filter((r) => r.group === name);
        if (items.length) out.push({ name, items });
    }
    // Действие с неизвестной группой не должно пропасть из шпаргалки.
    const known = new Set(order);
    const rest = rows.filter((r) => !known.has(r.group));
    if (rest.length) out.push({ name: 'Прочее', items: rest });
    return out;
}
