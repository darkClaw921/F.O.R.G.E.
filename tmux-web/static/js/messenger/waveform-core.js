// Звуковая дорожка голосового сообщения — ОДИН рисовальщик на три панели.
//
// Модуль появился в Ф2 фичи голосовых: до неё волну умел рисовать только VK
// (`js/vk/media.js`), Telegram всегда рисовал шум, а Bitrix не имел ветки
// настоящей огибающей вовсе. Копировать двадцать строк в третий раз значило бы
// завести три расходящихся вида одной и той же карточки — поэтому ветка
// вынесена сюда, а панели передают своё имя класса и свою «соль» для шума.
//
// # ДВА РЕЖИМА, И ВТОРОЙ — ТОЛЬКО ЗАГЛУШКА
//
// 1. `waveform` непустой — рисуем ЕГО. Это настоящая форма записи: у VK она
//    приходит в ответе, у Telegram — в `DocumentAttributeAudio.waveform`, у
//    Bitrix и у телеграмных аудиофайлов её считает бэкенд по скачанному файлу
//    (`forge_voice::waveform`).
// 2. Массив пуст — ДЕТЕРМИНИРОВАННЫЙ шум по идентификаторам сообщения. Он
//    существует ровно затем, чтобы карточка не «дышала» при каждой перерисовке
//    ленты и чтобы два голосовых одного сообщения выглядели по-разному. Как
//    только настоящая огибающая доедет, заглушка исчезает сама.
//
// # Почему `doc` — параметр
//
// Разметку карточек Bitrix строит на фейковом документе фронт-тестов
// (`bitrix_dom_stub.mjs`), где из всего DOM есть `createElement`/`appendChild`.
// Модуль обязан работать и там, поэтому глобальный `document` — лишь значение
// по умолчанию.

/** Сколько столбиков рисуем: и в шуме, и в прореженной настоящей волне. */
export const WAVE_BARS = 40;

/** Высота самого тихого столбика, пиксели (нулевой высоты не бывает). */
export const WAVE_MIN_PX = 8;

/** Насколько самый громкий столбик выше самого тихого, пиксели. */
export const WAVE_RANGE_PX = 24;

/**
 * «Соль» детерминированного шума.
 *
 * `index` пропускают панели, у которых вложение в сообщении одно (Telegram):
 * подмешивать в соль ноль означало бы изменить рисунок всех уже нарисованных
 * заглушек, а они у пользователя перед глазами каждый день.
 *
 * @param {number|string} messageId
 * @param {?number} [index] — номер вложения, если их бывает несколько
 * @returns {number}
 */
export function noiseSeed(messageId, index = null) {
    const base = (Number(messageId) || 1) * 2654435761;
    if (index == null) return base;
    return base ^ ((Number(index) || 0) + 1) * 40503;
}

/**
 * Высоты столбиков настоящей огибающей, пиксели.
 *
 * Нормализация — ПО МАКСИМУМУ САМОГО МАССИВА, а не по потолку шкалы: тихая
 * запись иначе рисовалась бы полоской у самого низа карточки. Прореживание
 * шагом `ceil(len / WAVE_BARS)` — массив бывает и в двести отсчётов, а
 * столбиков в карточке сорок.
 *
 * @param {Array<number>} waveform
 * @returns {Array<number>}
 */
export function realHeights(waveform) {
    const real = Array.isArray(waveform) ? waveform : [];
    if (!real.length) return [];
    const peak = real.reduce((acc, v) => Math.max(acc, Number(v) || 0), 0) || 1;
    const step = Math.max(1, Math.ceil(real.length / WAVE_BARS));
    const out = [];
    for (let i = 0; i < real.length; i += step) {
        out.push(WAVE_MIN_PX + Math.round(((Number(real[i]) || 0) / peak) * WAVE_RANGE_PX));
    }
    return out;
}

/**
 * Высоты столбиков заглушки, пиксели.
 *
 * xorshift32 от соли: одна и та же карточка всегда даёт одну и ту же картинку,
 * разные — разные.
 *
 * @param {number} seed
 * @returns {Array<number>}
 */
export function noiseHeights(seed) {
    let s = seed >>> 0 || 1;
    const out = [];
    for (let i = 0; i < WAVE_BARS; i += 1) {
        s = (s ^ (s << 13)) >>> 0;
        s = (s ^ (s >>> 17)) >>> 0;
        out.push(WAVE_MIN_PX + (s % WAVE_RANGE_PX));
    }
    return out;
}

/**
 * Высоты столбиков карточки: настоящая огибающая, если она есть, иначе шум.
 *
 * Чистая функция — на ней и держатся тесты рендера: проверять высоты честнее,
 * чем ходить по узлам.
 *
 * @param {?Array<number>} waveform
 * @param {number} seed
 * @returns {Array<number>}
 */
export function waveHeights(waveform, seed) {
    const real = realHeights(waveform);
    return real.length ? real : noiseHeights(seed);
}

/**
 * Готовый узел волны.
 *
 * @param {object} opts
 * @param {?Array<number>} [opts.waveform] — огибающая из DTO
 * @param {number} [opts.seed] — соль заглушки (см. [`noiseSeed`])
 * @param {string} opts.className — имя класса контейнера панели
 * @param {object} [opts.doc] — документ (фейковый в тестах)
 * @returns {object} узел
 */
export function buildWave({ waveform, seed = 1, className, doc = globalThis.document }) {
    const wave = doc.createElement('div');
    wave.className = className;
    // Волна — картинка, а не текст: скринридеру она не даёт ничего, а сорок
    // пустых `span` в озвучке — это сорок пауз посреди карточки.
    if (typeof wave.setAttribute === 'function') wave.setAttribute('aria-hidden', 'true');
    for (const height of waveHeights(waveform, seed)) {
        const bar = doc.createElement('span');
        bar.style.height = `${height}px`;
        wave.appendChild(bar);
    }
    return wave;
}

/** Есть ли у вложения настоящая огибающая (а не заглушка). */
export function hasRealWaveform(media) {
    return Array.isArray(media && media.waveform) && media.waveform.length > 0;
}
