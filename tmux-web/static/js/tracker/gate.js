// tmux-web — плагин forge-tracker: гейт вкладки.
//
// Модуль вынесен из js/tracker/main.js РАДИ ЛЕНИВОСТИ. Гейт обязан
// отработать на каждой загрузке страницы (иначе кнопка ✇ не появится), а
// панель целиком — нет: main.js тянет за собой рендер, сокет, разбор строки
// и карточку, и грузить всё это тем, кто в трекер сегодня не зайдёт, незачем.
// Поэтому здесь только один запрос и одна кнопка, а тяжёлая часть
// подгружается динамическим import'ом при первом открытии вкладки
// (js/tabs/tabs.js).
//
// ЕДИНСТВЕННЫЙ `GET /api/tracker/status` ЗА ЗАГРУЗКУ. Плагин opt-in: 404
// означает «devforge запущен без --tracker», это штатный путь, и в консоль
// при нём НЕ пишется ни строчки — иначе каждая загрузка страницы у всех, кто
// трекером не пользуется, давала бы красную строку.

import { $trackerToggle } from '../core/dom.js';
import { getStatus } from './api.js';

const local = {
    /** Гейт уже отработал. */
    gated: false,
    /** Ответ сервера при 200, иначе null. */
    status: null,
};

/**
 * Спросить сервер и показать кнопку вкладки при 200.
 *
 * Не бросает никогда: без плагина панель просто не существует.
 *
 * @returns {Promise<boolean>} включён ли плагин
 */
export async function initTrackerGate() {
    if (local.gated) return !!local.status;
    local.gated = true;
    const status = await getStatus();
    local.status = (status && status.enabled === true) ? status : null;
    if ($trackerToggle) $trackerToggle.hidden = !local.status;
    return !!local.status;
}

/** Ответ гейта (`null` — плагин выключен или гейт ещё не отработал). */
export function trackerStatus() {
    return local.status;
}

/** Включён ли плагин. */
export function isTrackerEnabled() {
    return !!local.status;
}
