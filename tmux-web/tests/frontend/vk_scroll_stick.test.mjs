// Регресс-тест «лента VK остаётся у последнего сообщения, когда содержимое
// дорастает уже ПОСЛЕ прокрутки» (js/vk/chat.js + js/vk/media.js).
//
// Стаб импортируется ПЕРВЫМ — см. пояснение в vk_dom_stub.mjs.
//
// ЧТО ЗА ДЕФЕКТ. Пользовательская приёмка: «чат автоматически не прокручивается
// до последнего сообщения». Причина воспроизведена в браузере на моке:
//
//   * `openDialog` рисует историю и синхронно зовёт `scrollToBottom()` —
//     в этот момент лента ДЕЙСТВИТЕЛЬНО внизу (отступ 0px);
//   * превью вложений грузятся ЛЕНИВО (IntersectionObserver в js/vk/media.js),
//     и каждое показанное превью растит `.vk-media-body` с min-height 92px до
//     max-height 184px — то есть лента дорастает уже ПОСЛЕ прокрутки;
//   * `scrollTop` при этом не меняется, и низ уезжает на 92px за карточку
//     (замер живого стенда: 8842 → 9210 при четырёх видимых превью, отступ
//     368px);
//   * дальше дефект самоусиливается: `isAtBottom` теперь ложно, поэтому ветка
//     `stick` в `onNewMessage` не срабатывает больше НИКОГДА.
//
// Отсюда две модели в этом тесте: «превью догрузилось» = прирост
// `scrollHeight` + событие `load` на `<img>` карточки. Ровно так это и
// выглядит в браузере: до декодирования картинки `<img>` занимает по вертикали
// ноль, тело карточки держится на min-height, и высота меняется в тот момент,
// когда браузер узнал размеры, — то есть к событию `load` раскладка уже новая.
//
// Проверяется:
//   1. ОТКРЫТИЕ БЕСЕДЫ: догрузка превью не оставляет ленту выше низа;
//   2. НОВОЕ СООБЩЕНИЕ: та же догрузка после `stick`-ветки;
//   3. САМОУСИЛЕНИЕ: после догрузки следующее сообщение по-прежнему прилипает;
//   4. РЕГРЕСС-ЩИТ: читающего историю рост НЕ дёргает вниз;
//   5. ВОЗВРАТ к нижней кромке снова включает прижатие;
//   6. ПОДГРУЗКА ВВЕРХ: рост во время неё не сбивает якорь чтения.
//
// Запуск: node tmux-web/tests/frontend/vk_scroll_stick.test.mjs

import {
    $log, dispatch, flush, jsonRes, mediaRes, setNet, showObserved,
} from './vk_dom_stub.mjs';

const C = await import(new URL('../../static/js/vk/chat.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
const RL = await import(new URL('../../static/js/vk/ratelimit.js', import.meta.url).href);

// Планировщик 3 rps проверяется своим тестом (vk_ratelimit): здесь он заставил
// бы каждый запрос ждать настоящие 380 мс.
RL.vkLimits.minIntervalMs = 1;
RL.vkLimits.burst = 100;

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

const settle = async (ms) => {
    await new Promise((r) => setTimeout(r, ms === undefined ? 5 : ms));
    await flush();
};

/**
 * Насколько тело карточки вырастает, когда превью догрузилось.
 *
 * `.vk-media-body`: min-height 92px (иконка вида вложения) → max-height 184px
 * (картинка). Число зашито в css/vk.css и здесь только моделируется.
 */
const THUMB_GROWTH_PX = 92;

/**
 * Обрезка `scrollTop` потолком прокрутки — как в браузере.
 *
 * Фейковый DOM хранит присвоенное значение как есть, и `scrollTop =
 * scrollHeight` оставляло бы ленту «за низом»: любой последующий прирост
 * высоты она бы «переваривала» бесплатно, и отличить прижатую ленту от
 * уехавшей стало бы нечем. Ровно на этой разнице и держится дефект — модель
 * без потолка проверяла бы не то.
 */
{
    let rawTop = 0;
    Object.defineProperty($log, 'scrollTop', {
        get: () => rawTop,
        set(v) {
            const max = Math.max(0, $log.scrollHeight - $log.clientHeight);
            rawTop = Math.max(0, Math.min(Number(v) || 0, max));
        },
    });
}

/**
 * Геометрия ленты: браузерной раскладки нет, тест выставляет её руками.
 *
 * Размеры ставятся ДО `scrollTop`: потолок прокрутки считается по ним, и
 * обратный порядок обрезал бы позицию по геометрии прошлого сценария.
 */
function layout(scrollTop, scrollHeight, clientHeight) {
    $log.scrollHeight = scrollHeight === undefined ? 1000 : scrollHeight;
    $log.clientHeight = clientHeight === undefined ? 200 : clientHeight;
    $log.scrollTop = scrollTop;
}

/** Отступ последнего сообщения от нижней кромки (0 ⇒ лента внизу). */
const gap = () => $log.scrollHeight - $log.clientHeight - $log.scrollTop;

/** Все превью, которым уже подставлен blob (то есть ждущие декодирования). */
const pendingThumbs = () => $log.querySelectorAll('.vk-media-thumb')
    .filter((t) => !t.hidden && t.getAttribute('src'));

/**
 * Изображает декодирование превью: лента дорастает, затем прилетает `load`.
 *
 * Порядок именно такой, как в браузере: к моменту события раскладка уже
 * пересчитана, и обработчик, читающий `scrollHeight`, видит НОВУЮ высоту.
 *
 * @returns {number} сколько превью «догрузилось»
 */
function decodeThumbs() {
    const thumbs = pendingThumbs();
    for (const img of thumbs) {
        $log.scrollHeight += THUMB_GROWTH_PX;
        dispatch(img, 'load', { target: img });
    }
    return thumbs.length;
}

/** Вложение-картинка в форме MediaDto бэкенда. */
function photo(index) {
    return {
        index,
        kind: 'photo',
        mime: 'image/jpeg',
        size: 120000,
        file_name: `photo${index}.jpg`,
        title: null,
        width: 1280,
        height: 960,
        duration_secs: null,
        has_thumb: true,
        has_full: true,
        player_url: null,
        waveform: [],
    };
}

function message(id, attachments, extra) {
    const atts = attachments || [];
    return Object.assign({
        id,
        dialog_id: '2000000007',
        out: false,
        date: 1_700_000_000 + id * 60,
        sender: { id: '1234', first_name: 'Павел', screen_name: 'durov' },
        text: `сообщение ${id}`,
        media: atts[0] || null,
        attachments: atts,
        reply_to_id: null,
        reply_to: null,
        edit_date: null,
        reply_markup: null,
        cmid: id - 6000,
        fwd: [],
        action: null,
        payload: null,
    }, extra || {});
}

const DIALOG = { id: '2000000007', kind: 'chat', title: 'Проектная' };
S.vkState.dialogs = [DIALOG];
S.vkState.selfUser = { id: '999' };
C.initChat({ onRead: () => {}, presenceOf: () => null });

/** История + байты медиа-прокси; всё остальное — пустой JSON. */
const net = (msgs, hasMore) => async (url, init) => {
    if (url.includes('/api/vk/media/')) return mediaRes([1, 2, 3, 4], 'image/jpeg');
    if (init && init.method === 'POST') return jsonRes(200, { ok: true });
    if (url.includes('/messages')) return jsonRes(200, { messages: msgs, has_more: !!hasMore });
    return jsonRes(200, {});
};

async function open(msgs, hasMore) {
    setNet(net(msgs, hasMore));
    await C.openDialog('2000000007', DIALOG);
    await settle();
    // Браузер шлёт `scroll` и на ПРОГРАММНУЮ прокрутку — фейковый DOM сам не
    // умеет. Без этого события лента запомнила бы прошлое положение, и
    // «пользователь ушёл вверх» ниже сравнивалось бы с чужим числом.
    dispatch($log, 'scroll');
}

// История на несколько экранов; каждое четвёртое сообщение — с вложениями.
const HISTORY = Array.from({ length: 40 }, (_, k) => {
    const id = 7001 + k;
    return message(id, id % 4 === 0 ? [photo(0), photo(1), photo(2)] : []);
});

// ---------------------------------------------------------------------------
// 1. Открытие беседы: догрузка превью не оставляет ленту выше низа
// ---------------------------------------------------------------------------

{
    layout(0, 4000, 700);
    await open(HISTORY, false);
    // Сама прокрутка при открытии работала и до починки — фиксируем это, чтобы
    // отличать «не прокрутилось вовсе» от «уехало потом».
    eq(gap() <= 0, true, 'сразу после renderHistory лента у нижней кромки');

    // Карточки подъехали к вьюпорту — IntersectionObserver отдал их загрузке.
    showObserved();
    await settle();
    const grown = decodeThumbs();
    ok(grown > 0, `превью действительно догрузились (${grown} шт.)`);

    eq(gap() <= 0, true,
        `догрузка превью не должна отрывать ленту от низа (отступ ${gap()}px)`);
    eq($log.scrollTop, $log.scrollHeight - $log.clientHeight, 'лента прижата к самому низу');
}

// ---------------------------------------------------------------------------
// 1a. Событие прокрутки приходит ПОЗЖЕ роста — защёлка обязана уцелеть
// ---------------------------------------------------------------------------

{
    // Самая тонкая часть починки. `scroll` доставляется асинхронно, и к моменту
    // доставки лента успевает дорасти ещё раз — например превью соседней
    // карточки или подменённым шрифтом, о котором ленте никто не сообщает.
    // Наивное правило «прижата ⇔ isAtBottom прямо сейчас» на этом событии
    // прочитало бы «пользователь ушёл вверх», хотя руки его ленты не касались,
    // — и следующий прирост уже не догнали бы. Именно так дефект и выглядел в
    // браузере: отступ залипал на 92px (высота одного превью).
    ok(C._debugChatState().stickBottom, 'исходно лента прижата');
    const before = $log.scrollTop;
    $log.scrollHeight += 200;
    dispatch($log, 'scroll');
    ok(C._debugChatState().stickBottom,
        'отложенное событие прокрутки не снимает прижатие: рука пользователя ленты не касалась');

    // И следующий прирост по-прежнему догоняется.
    $log.scrollHeight += 92;
    C.onMediaGrew();
    eq(gap() <= 0, true, 'лента догнала прирост, случившийся после события');
    ok($log.scrollTop > before, 'и действительно уехала вниз');
}

// ---------------------------------------------------------------------------
// 1b. Прокрутка вниз объявляет НАМЕРЕНИЕ, а не только двигает scrollTop
// ---------------------------------------------------------------------------

{
    // Пользователь ушёл читать историю — прижатие снято.
    layout(400, 4000, 700);
    dispatch($log, 'scroll');
    eq(C._debugChatState().stickBottom, false, 'чтение истории прижатие снимает');

    // Открытие беседы — намерение «показать конец переписки». Событие
    // прокрутки после этого НАМЕРЕННО не отправляется: защёлка обязана
    // держаться на самом факте доскролла, а не на том, успел ли браузер
    // прислать `scroll` раньше, чем догрузилось первое превью.
    setNet(net(HISTORY, false));
    await C.openDialog('2000000007', DIALOG);
    await settle();
    eq(C._debugChatState().stickBottom, true, 'открытие беседы прижимает ленту к низу');
    dispatch($log, 'scroll');
}

// ---------------------------------------------------------------------------
// 2. Новое сообщение: та же догрузка после ветки stick
// ---------------------------------------------------------------------------

{
    layout(3300, 4000, 700);
    C.onNewMessage(message(7500, [photo(0)]));
    eq(gap() <= 0, true, 'новое сообщение у нижней кромки прилипает (прежнее поведение)');

    showObserved();
    await settle();
    ok(decodeThumbs() > 0, 'превью нового сообщения догрузилось');
    eq(gap() <= 0, true,
        `догрузка превью нового сообщения тоже не отрывает ленту (отступ ${gap()}px)`);
}

// ---------------------------------------------------------------------------
// 3. Самоусиление: после догрузки следующее сообщение по-прежнему прилипает
// ---------------------------------------------------------------------------

{
    // Ровно тот сценарий, в котором дефект становился необратимым: первая
    // догрузка уводила ленту от низа, `isAtBottom` начинал врать, и ветка
    // `stick` не срабатывала уже никогда.
    eq(gap() <= 0, true,
        'после первой догрузки лента всё ещё у кромки — значит stick вообще сработает');
    const before = $log.scrollTop;
    C.onNewMessage(message(7501, [photo(0)]));
    showObserved();
    await settle();
    ok(decodeThumbs() > 0, 'превью следующего сообщения тоже догрузилось');
    ok($log.scrollTop > before, 'лента уехала вниз за новым сообщением');
    eq(gap() <= 0, true, 'stick продолжает работать после догрузки превью');
}

// ---------------------------------------------------------------------------
// 4. Регресс-щит: читающего историю рост НЕ дёргает вниз
// ---------------------------------------------------------------------------

{
    await open(HISTORY, false);
    layout(1200, 4000, 700);
    // Пользователь ушёл читать историю — событие прокрутки об этом сообщает.
    dispatch($log, 'scroll');

    showObserved();
    await settle();
    const grown = decodeThumbs();
    ok(grown > 0, 'превью догрузились и в истории тоже');
    eq($log.scrollTop, 1200,
        'лента читающего историю остаётся на месте, сколько бы ни доросла');
}

// ---------------------------------------------------------------------------
// 5. Возврат к нижней кромке снова включает прижатие
// ---------------------------------------------------------------------------

{
    // Вернулись вниз руками — прижатие обязано восстановиться, иначе после
    // одного взгляда в историю лента переставала бы следить за перепиской.
    $log.scrollTop = $log.scrollHeight - $log.clientHeight;
    dispatch($log, 'scroll');

    C.onNewMessage(message(7502, [photo(0)]));
    showObserved();
    await settle();
    ok(decodeThumbs() > 0, 'превью догрузилось после возврата вниз');
    eq(gap() <= 0, true, 'вернувшемуся вниз лента снова прижата');
}

// ---------------------------------------------------------------------------
// 6. Подгрузка вверх: рост во время неё не сбивает якорь чтения
// ---------------------------------------------------------------------------

{
    await open(HISTORY, true);
    // Пользователь у верхней кромки — сейчас поедет страница истории.
    layout(0, 4000, 700);
    dispatch($log, 'scroll');

    const older = Array.from({ length: 10 }, (_, k) => message(6900 + k, [photo(0)]));
    setNet(async (url, init) => {
        if (url.includes('/api/vk/media/')) return mediaRes([1, 2, 3, 4], 'image/jpeg');
        if (init && init.method === 'POST') return jsonRes(200, { ok: true });
        if (url.includes('before_id=')) {
            // Пока летит страница, догружается превью уже нарисованной карточки.
            $log.scrollHeight += THUMB_GROWTH_PX;
            for (const img of pendingThumbs()) dispatch(img, 'load', { target: img });
            return jsonRes(200, { messages: older, has_more: false });
        }
        return jsonRes(200, { messages: HISTORY, has_more: true });
    });
    // Модель прироста высоты от вставленной страницы.
    const prevHeight = $log.scrollHeight;
    await C.loadOlder(DIALOG);
    $log.scrollHeight = prevHeight + 1500;
    await settle(20);

    ok($log.scrollTop < $log.scrollHeight - $log.clientHeight,
        `подгрузка вверх не швыряет ленту в самый низ (scrollTop ${$log.scrollTop})`);
}

console.log(`vk_scroll_stick: ${checks} проверок, ${failures} провалов`);
process.exit(failures === 0 ? 0 : 1);
