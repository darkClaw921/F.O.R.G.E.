// РЕЕСТР СЛОЁВ Esc — один слушатель на документ, одно нажатие = одно окно.
//
// ЗАЧЕМ ОН ЕСТЬ (forge-o0sdg). Карточка встречи, попап анализа чата и полоса
// цитаты ответа вешали СВОИ слушатели `keydown` на ОДИН И ТОТ ЖЕ `document` и
// каждый защищался вызовом `stopPropagation()`. Это не работает: между
// слушателями одного узла `stopPropagation()` не действует вовсе — он
// останавливает всплытие ВЫШЕ по дереву, а остальных слушателей этого же узла
// не отменяет (для них существует `stopImmediatePropagation()`). Живая приёмка
// в Chrome показала закрытие ДВУХ окон одним Esc во всех трёх панелях.
//
// ПОЧЕМУ НЕ `stopImmediatePropagation()`. Он отменяет только тех слушателей,
// которые зарегистрированы ПОЗЖЕ, то есть делает поведение заложником порядка
// загрузки модулей: сегодня карточка регистрируется после анализа, завтра
// порядок импортов в `bootstrap.js` меняется — и Esc начинает закрывать не то
// окно. Тест на это не покраснеет (он ставит слушателей в «правильном»
// порядке), а человек увидит дефект сразу.
//
// КАК УСТРОЕНО ЗДЕСЬ. Слои объявляют СТАТИЧЕСКУЮ ГЛУБИНУ (`ESC_DEPTH`),
// повторяющую реальный визуальный порядок наслоения, и общий слушатель
// закрывает САМЫЙ ГЛУБОКИЙ из ОТКРЫТЫХ. Ни порядок регистрации, ни порядок
// открытия ни на что не влияют, а третий и четвёртый слой добавляются одной
// строкой.
//
// ГРАНИЦЫ. Реестр — власть над ОКНАМИ ПАНЕЛИ (карточка встречи, попап анализа,
// полоса ответа). Лайтбокс, плеер, мобильная колонка бесед и оверлей captcha
// слушают Esc сами и остаются со своими гардами: их взаимное поведение
// сложилось раньше и трогать его этим исправлением незачем. Поэтому общий
// слушатель НЕ зовёт `stopImmediatePropagation()` — он лишь `preventDefault()`
// и `stopPropagation()` (последнее было и в карточке: без него Esc всплывал бы
// до `window` и закрывал заодно мобильную боковую колонку). Слой, который
// обязан уступить не-реестровому окну, говорит об этом своим `isOpen()` — так
// делает полоса ответа при открытом лайтбоксе.
//
// Слушатель заводится ЛЕНИВО при первом слое и снимается, когда снят
// последний: переживший выгрузку страницы в bfcache слушатель стреляет по
// мёртвому DOM (этот дефект в проекте уже ловили).

/**
 * Глубины слоёв — визуальный порядок наслоения, а не приоритет вкуса.
 *
 * Карточка встречи вставляется ПОСЛЕДНИМ потомком панели и при равном
 * `z-index` ложится поверх попапа анализа; полоса цитаты — часть формы ввода и
 * лежит ниже обоих.
 */
export const ESC_DEPTH = Object.freeze({
    REPLY_BAR: 10,
    ANALYSIS_POPUP: 20,
    MEET_CARD: 30,
});

/** Реестры по документам: у тестов свой фейковый документ на файл. */
const registries = new Map();

function registryFor(doc) {
    let reg = registries.get(doc);
    if (reg) return reg;
    reg = { doc, layers: [], handler: null };
    reg.handler = (e) => onKeyDown(reg, e);
    if (doc && typeof doc.addEventListener === 'function') {
        doc.addEventListener('keydown', reg.handler);
    }
    registries.set(doc, reg);
    return reg;
}

function dropRegistry(reg) {
    if (reg.layers.length) return;
    if (reg.doc && typeof reg.doc.removeEventListener === 'function') {
        reg.doc.removeEventListener('keydown', reg.handler);
    }
    registries.delete(reg.doc);
}

/** Самый глубокий из ОТКРЫТЫХ слоёв (при равной глубине — зарегистрированный позже). */
function topOf(reg) {
    let best = null;
    for (const layer of reg.layers) {
        let open = false;
        try {
            open = !!layer.isOpen();
        } catch (_) {
            // Слой, который не может ответить, считается закрытым: Esc обязан
            // достаться следующему, а не пропасть вместе с исключением.
            open = false;
        }
        if (!open) continue;
        if (!best || layer.depth >= best.depth) best = layer;
    }
    return best;
}

function onKeyDown(reg, e) {
    if (!e || e.key !== 'Escape') return;
    const top = topOf(reg);
    // Ни одного открытого слоя — событие принадлежит не нам (лайтбокс, плеер,
    // мобильная колонка). Ни `preventDefault`, ни остановки: иначе Esc пропал
    // бы у соседей.
    if (!top) return;
    if (typeof e.preventDefault === 'function') e.preventDefault();
    if (typeof e.stopPropagation === 'function') e.stopPropagation();
    top.close();
}

/**
 * Зарегистрировать окно как слой Esc.
 *
 * @param {{
 *   name: string,
 *   depth: number,
 *   isOpen: function(): boolean,
 *   close: function(): void,
 *   doc?: object,
 * }} layer
 * @returns {function(): void} снятие слоя (идемпотентно)
 */
export function registerEscLayer(layer) {
    const l = layer || {};
    if (typeof l.isOpen !== 'function' || typeof l.close !== 'function') {
        throw new TypeError('registerEscLayer: нужны isOpen() и close()');
    }
    const doc = l.doc || (typeof document !== 'undefined' ? document : null);
    if (!doc) return () => {};
    const reg = registryFor(doc);
    const entry = {
        name: String(l.name || 'layer'),
        depth: Number.isFinite(l.depth) ? Number(l.depth) : ESC_DEPTH.ANALYSIS_POPUP,
        isOpen: l.isOpen,
        close: l.close,
    };
    reg.layers.push(entry);
    let removed = false;
    return () => {
        if (removed) return;
        removed = true;
        const i = reg.layers.indexOf(entry);
        if (i >= 0) reg.layers.splice(i, 1);
        dropRegistry(reg);
    };
}

/**
 * Верхний ОТКРЫТЫЙ слой документа — или `null`.
 *
 * Нужен тем обработчикам Esc, которые слоем стать не могут, потому что висят на
 * поле ввода и зависят от фокуса (режим ответа/правки в Битриксе): им довольно
 * промолчать, пока сверху есть окно.
 *
 * @param {object} [doc]
 * @returns {?{name: string, depth: number}}
 */
export function topEscLayer(doc) {
    const target = doc || (typeof document !== 'undefined' ? document : null);
    const reg = registries.get(target);
    if (!reg) return null;
    const top = topOf(reg);
    return top ? { name: top.name, depth: top.depth } : null;
}

/** Снимок реестра — только для тестов и отладки. */
export function _debugEscStack(doc) {
    const target = doc || (typeof document !== 'undefined' ? document : null);
    const reg = registries.get(target);
    if (!reg) return { layers: [], bound: false };
    return {
        bound: true,
        layers: reg.layers.map((l) => ({ name: l.name, depth: l.depth })),
    };
}
