// Регресс-тест карточек вложений панели VK (js/vk/media.js) на фейковом DOM.
//
// Стаб импортируется ПЕРВЫМ — см. пояснение в vk_dom_stub.mjs.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ (всё это невозможно у Telegram, где вложение одно):
//
//   1. НЕСКОЛЬКО КАРТОЧЕК НА СООБЩЕНИЕ. У VK вложений до десяти; строка ленты
//      обязана нарисовать группу карточек, а хвост сверх MAX_INLINE_CARDS —
//      свернуть в «…ещё N»;
//   2. КЛЮЧ КЭША С ИНДЕКСОМ. Без индекса второе фото затирало бы первое и в
//      vkState.mediaCache, и в учёте objectURL — то есть один блоб остался бы
//      неотозванным навсегда, а прогресс второго фото рисовался бы на первом;
//   3. ГАЛЕРЕЯ В ЛАЙТБОКСЕ. Стрелки ←/→ ходят по картиночным вложениям ОДНОГО
//      сообщения и не заворачиваются по кругу;
//   4. REVOKE ПРИ СМЕНЕ БЕСЕДЫ. Проверяется СЧЁТЧИКОМ созданных и отозванных
//      objectURL, а не «вроде бы вызвали revoke»;
//   5. НИ ОДНОГО ПРЯМОГО URL VK В DOM. Всё идёт через прокси бэкенда
//      (/api/vk/media/...), кроме страницы плеера видео — и она через safeUrl;
//   6. ЛЕНИВЫЕ ПРЕВЬЮ: до появления карточки в вьюпорте запроса НЕТ.
//
// Запуск: node tmux-web/tests/frontend/vk_media.test.mjs

import {
    $lightbox, $lightboxCaption, $lightboxImg, $lightboxNext, $lightboxPrev, $log,
    dispatch, flush, jsonRes, mediaRes, netCalls, objectUrls, setNet, showObserved,
} from './vk_dom_stub.mjs';

const M = await import(new URL('../../static/js/vk/media.js', import.meta.url).href);
const C = await import(new URL('../../static/js/vk/chat.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
const F = await import(new URL('../../static/js/vk/format.js', import.meta.url).href);
const R = await import(new URL('../../static/js/vk/ratelimit.js', import.meta.url).href);

// Планировщик 3 rps проверяется своим тестом (vk_ratelimit): здесь он заставил
// бы каждый запрос ждать настоящие 380 мс.
R.vkLimits.minIntervalMs = 1;
R.vkLimits.burst = 100;

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

/** Вложение в форме MediaDto бэкенда (snake_case — как в JSON). */
function att(index, kind, extra) {
    return Object.assign({
        index,
        kind,
        mime: kind === 'photo' ? 'image/jpeg' : null,
        size: 1024 * (index + 1),
        file_name: null,
        title: null,
        width: 1280,
        height: 720,
        duration_secs: null,
        has_thumb: kind !== 'voice' && kind !== 'audio' && kind !== 'other',
        has_full: kind !== 'video' && kind !== 'other',
        player_url: null,
        link_url: null,
        waveform: [],
    }, extra || {});
}

function message(id, attachments, extra) {
    return Object.assign({
        id,
        dialog_id: '1234',
        out: false,
        date: 1700000000 + id,
        sender: { id: '1234', first_name: 'Павел', screen_name: 'durov' },
        text: 'смотри',
        media: attachments[0] || null,
        attachments,
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

const DIALOG = { id: '1234', kind: 'user', title: 'Павел Дуров', username: 'durov' };
S.vkState.dialogs = [DIALOG];
S.vkState.activeDialogId = '1234';

/** Все карточки внутри контейнера. */
const cardsOf = (box) => box.querySelectorAll('.vk-media-card');

// Медиа-прокси отдаёт байты, остальное — пустой JSON.
setNet(async (url) => {
    if (url.includes('/api/vk/media/')) return mediaRes([1, 2, 3, 4], 'image/jpeg');
    return jsonRes(200, {});
});

// ---------------------------------------------------------------------------
// 1. Группа карточек: одно сообщение — несколько вложений.
// ---------------------------------------------------------------------------

const three = message(7001, [att(0, 'photo'), att(1, 'photo'), att(2, 'document', {
    file_name: 'смета.pdf',
    mime: 'application/pdf',
})]);
const group3 = M.renderAttachmentGroup(three);
ok(group3 !== null, 'группа вложений построена');
eq(cardsOf(group3).length, 3, 'три вложения — три карточки');
eq(
    cardsOf(group3).map((c) => c.dataset.index),
    ['0', '1', '2'],
    'индексы карточек идут подряд и совпадают с индексами вложений'
);
eq(
    cardsOf(group3).map((c) => c.dataset.mediaKind),
    ['photo', 'photo', 'document'],
    'вид вложения помечен на карточке'
);
ok(
    cardsOf(group3)[2].textContent.includes('смета.pdf'),
    'имя файла документа попало в карточку'
);

// Хвост сверх MAX_INLINE_CARDS сворачивается.
const many = message(7002, Array.from({ length: 9 }, (_, i) => att(i, 'photo')));
const group9 = M.renderAttachmentGroup(many);
eq(cardsOf(group9).length, 6, 'карточек не больше MAX_INLINE_CARDS');
const more = group9.querySelector('.vk-media-more');
ok(more !== null && more.textContent.includes('…ещё 3'), `хвост: ${more && more.textContent}`);

// Сообщение без вложений группы не даёт вовсе.
eq(M.renderAttachmentGroup(message(7003, [])), null, 'нет вложений — нет контейнера');

// ---------------------------------------------------------------------------
// 2. Ленивые превью и КЛЮЧИ КЭША С ИНДЕКСОМ.
// ---------------------------------------------------------------------------

M.resetMediaCards();
M.revokeDialogMedia();
netCalls.length = 0;

const pair = message(7010, [att(0, 'photo'), att(1, 'photo')]);
const groupPair = M.renderAttachmentGroup(pair);
await settle();
eq(
    netCalls.filter((c) => c.url.includes('/api/vk/media/')).length,
    0,
    'до появления в вьюпорте превью НЕ грузятся (иначе сотня медиа = сотня запросов)'
);

showObserved();
await settle();
const thumbCalls = netCalls.filter((c) => c.url.includes('/api/vk/media/')).map((c) => c.url);
eq(thumbCalls.length, 2, 'каждая карточка запросила СВОЁ превью');
ok(
    thumbCalls.some((u) => u.includes('/api/vk/media/1234/7010/0?kind=thumb'))
    && thumbCalls.some((u) => u.includes('/api/vk/media/1234/7010/1?kind=thumb')),
    `адреса превью различаются индексом: ${thumbCalls.join(' | ')}`
);

// ГЛАВНОЕ: ключи кэша разные, и блобов ровно столько же.
const keys = Array.from(S.vkState.mediaCache.keys());
eq(keys.length, 2, 'в кэше две записи, а не одна');
eq(
    keys.sort(),
    ['1234/7010/0/thumb', '1234/7010/1/thumb'],
    'ключ кэша НЕСЁТ ИНДЕКС — без него второе фото затёрло бы первое'
);
eq(
    F.mediaKey('1234', 7010, 0, 'thumb') === F.mediaKey('1234', 7010, 1, 'thumb'),
    false,
    'mediaKey различает индексы'
);
const urls = Array.from(S.vkState.mediaCache.values());
eq(new Set(urls).size, 2, 'два разных objectURL');

// Превью подставлено в <img>, и это blob:, а не адрес VK.
const thumbs = groupPair.querySelectorAll('.vk-media-thumb');
eq(thumbs.length, 2, 'у обеих карточек есть узел превью');
ok(thumbs.every((t) => String(t.getAttribute('src')).startsWith('blob:')),
    `превью грузится через fetch→blob (иначе Bearer не доедет): ${thumbs.map((t) => t.getAttribute('src'))}`);
ok(thumbs.every((t) => t.hidden === false), 'превью показаны');

// ---------------------------------------------------------------------------
// 3. Прогресс адресуется ИНДЕКСОМ.
// ---------------------------------------------------------------------------

M.onDownloadProgress({
    dialog_id: '1234', message_id: 7010, index: 1, kind: 'full', received: 512, total: 1024,
});
const statuses = groupPair.querySelectorAll('.vk-media-status').map((n) => n.textContent);
eq(statuses[0], '', 'кадр про второе вложение НЕ трогает первую карточку');
ok(statuses[1].startsWith('VK '), `вторая карточка показывает прогресс: ${statuses[1]}`);

// Кадр про превью карточку не мигает.
M.onDownloadProgress({
    dialog_id: '1234', message_id: 7010, index: 0, kind: 'thumb', received: 10, total: 100,
});
eq(
    groupPair.querySelectorAll('.vk-media-status')[0].textContent,
    '',
    'прогресс превью не рисуется — оно и так мгновенное'
);

// ---------------------------------------------------------------------------
// 4. Лайтбокс-ГАЛЕРЕЯ.
// ---------------------------------------------------------------------------

M.initMedia();
const gallery = message(7020, [att(0, 'photo'), att(1, 'document'), att(2, 'photo'), att(3, 'photo')]);
const groupG = M.renderAttachmentGroup(gallery);
showObserved();
await settle();
const gCards = cardsOf(groupG);
// Открываем ВТОРОЕ фото (индекс 2): галерея — только картиночные вложения.
// Клик именно по ПРЕВЬЮ: по generic-иконке карточка открываться не обязана —
// туда попадают случайно чаще, чем осознанно.
dispatch(gCards[2].querySelector('.vk-media-body'), 'click', { target: gCards[2].querySelector('.vk-media-body') });
await settle();

eq($lightbox.hidden, false, 'лайтбокс открыт');
let dbg = M._debugMediaState();
eq(dbg.gallery, 3, 'в галерее три КАРТИНОЧНЫХ вложения (документ выпал)');
eq(dbg.lightboxPos, 1, 'позиция — та карточка, по которой кликнули');
ok($lightboxCaption.textContent.includes('2/3'), `счётчик галереи: ${$lightboxCaption.textContent}`);
eq($lightboxPrev.hidden, false, 'стрелка «назад» видна при галерее');
eq($lightboxNext.hidden, false, 'стрелка «вперёд» видна при галерее');
ok(String($lightboxImg.getAttribute('src')).startsWith('blob:'), 'картинка — blob, а не адрес VK');

// Шаг вперёд и назад.
eq(M.stepLightbox(1), true, 'шаг вперёд выполнен');
await settle();
eq(M._debugMediaState().lightboxPos, 2, 'позиция сдвинулась');
eq($lightboxNext.disabled, true, 'на последнем вложении «вперёд» отключена');
eq(M.stepLightbox(1), false, 'за последним вложением галерея НЕ заворачивается по кругу');

dispatch($lightboxPrev, 'click', { target: $lightboxPrev });
await settle();
eq(M._debugMediaState().lightboxPos, 1, 'клик по стрелке листает назад');

dispatch($lightboxPrev, 'click', { target: $lightboxPrev });
await settle();
eq(M._debugMediaState().lightboxPos, 0, 'дошли до начала');
eq($lightboxPrev.disabled, true, 'на первом вложении «назад» отключена');
eq(M.stepLightbox(-1), false, 'перед первым вложением галереи нет');

// Esc закрывает.
dispatch(globalThis.document, 'keydown', { key: 'Escape', target: $lightbox });
eq($lightbox.hidden, true, 'Esc закрывает лайтбокс');
eq($lightboxPrev.hidden, true, 'стрелки спрятаны вместе с лайтбоксом');

// Одинокая картинка галереи не образует.
const single = message(7021, [att(0, 'photo')]);
const groupS = M.renderAttachmentGroup(single);
showObserved();
await settle();
const sCard = cardsOf(groupS)[0];
dispatch(sCard.querySelector('.vk-media-body'), 'click', { target: sCard.querySelector('.vk-media-body') });
await settle();
dbg = M._debugMediaState();
eq(dbg.gallery, 1, 'одно вложение — галереи нет');
eq($lightboxPrev.hidden, true, 'и стрелок тоже');
ok(!$lightboxCaption.textContent.includes('1/1'), 'счётчик «1/1» не рисуется');
M.closeLightbox();

// ---------------------------------------------------------------------------
// 5. Видео: плеер во внешней вкладке, файла нет.
// ---------------------------------------------------------------------------

const opened = [];
globalThis.window.open = (url, target, features) => {
    opened.push({ url, target, features });
    return { opener: null };
};

const video = message(7030, [att(0, 'video', {
    has_full: false,
    player_url: 'https://vk.com/video_ext.php?oid=1&id=2',
    duration_secs: 95,
    title: 'ролик',
})]);
const groupV = M.renderAttachmentGroup(video);
const vCard = cardsOf(groupV)[0];
const vButtons = vCard.querySelectorAll('.vk-media-btn');
eq(vButtons[1].disabled, true, 'SAVE у видео отключён — файла VK не отдаёт');
eq(vButtons[0].disabled, false, 'OPEN у видео работает: он ведёт на страницу плеера');

dispatch(vButtons[0], 'click', { target: vButtons[0] });
await settle();
eq(opened.length, 1, 'видео открылось во внешней вкладке');
eq(opened[0].url, 'https://vk.com/video_ext.php?oid=1&id=2', 'адрес — страница плеера VK');
eq(opened[0].features, 'noopener,noreferrer', 'без opener и без Referer');
eq($lightbox.hidden, true, 'видео НЕ открывается лайтбоксом');

// Подделанная схема отвергается.
const evil = message(7031, [att(0, 'video', {
    has_full: false,
    player_url: 'javascript:alert(1)',
})]);
const eCard = cardsOf(M.renderAttachmentGroup(evil))[0];
const eOpen = eCard.querySelectorAll('.vk-media-btn')[0];
eq(eOpen.disabled, true, 'видео с недопустимой схемой открывать нечем');
dispatch(eOpen, 'click', { target: eOpen });
await settle();
eq(opened.length, 1, 'javascript: во внешнюю вкладку не уходит');

// ---------------------------------------------------------------------------
// 6. Карточка other: заголовок без превью и без файла.
// ---------------------------------------------------------------------------

const other = message(7040, [att(0, 'other', {
    has_thumb: false,
    has_full: false,
    title: 'Опрос: когда релиз?',
    size: null,
})]);
const oCard = cardsOf(M.renderAttachmentGroup(other))[0];
ok(oCard.textContent.includes('Опрос: когда релиз?'), `заголовок в карточке: ${oCard.textContent}`);
eq(oCard.querySelector('.vk-media-thumb').hidden, true, 'превью у опроса нет');
const oButtons = oCard.querySelectorAll('.vk-media-btn');
eq([oButtons[0].disabled, oButtons[1].disabled], [true, true], 'открывать и сохранять нечего');
eq(oCard.querySelector('.vk-media-link').hidden, true, 'у опроса нет адреса — нет и подписи');
await settle();
eq(
    netCalls.filter((c) => c.url.includes('/api/vk/media/1234/7040/')).length,
    0,
    'карточка без файла не делает ни одного запроса'
);

// ---------------------------------------------------------------------------
// 6b. КАРТОЧКА ССЫЛКИ — РЕГРЕСС ЖИВОГО ДЕФЕКТА.
//
// Пользователь получил в беседе ссылку на bitrix24. Карточка нарисовала
// заголовок, метку DATA, размеры превью 1200×630 — и значок-заглушку вместо
// самой картинки. Причина: canShowThumb держал свой белый список видов, в
// который `other` не попал, поэтому запрос за превью не уходил ВООБЩЕ.
//
// Здесь проверяется всё, чего у карточки ссылки не было:
//   * превью запрашивается — и именно у прокси бэкенда, а не у VK;
//   * на карточке виден ДОМЕН (полный адрес ломает раскладку);
//   * по домену и по OPEN можно перейти, в новой вкладке и без opener/Referer;
//   * недопустимая схема отбрасывается — адрес набирал собеседник.
// ---------------------------------------------------------------------------

M.resetMediaCards();
netCalls.length = 0;
opened.length = 0;

const linkMsg = message(7045, [att(0, 'other', {
    has_thumb: true,
    has_full: false,
    title: 'Войти в Битрикс24',
    width: 1200,
    height: 630,
    size: null,
    link_url: 'https://psph.bitrix24.ru/crm/contact/details/27307/',
})]);
const lCard = cardsOf(M.renderAttachmentGroup(linkMsg))[0];
ok(lCard.textContent.includes('Войти в Битрикс24'), `заголовок ссылки: ${lCard.textContent}`);
ok(lCard.textContent.includes('1200×630'), `размеры превью на карточке: ${lCard.textContent}`);

const lLink = lCard.querySelector('.vk-media-link');
ok(lLink !== null, 'на карточке ссылки есть узел адреса');
eq(lLink.hidden, false, 'и он показан');
eq(lLink.textContent, 'psph.bitrix24.ru', 'показан ДОМЕН, а не полный адрес');
ok(
    !lLink.textContent.includes('27307'),
    `путь ссылки на карточку не едет: ${lLink.textContent}`
);
eq(
    lLink.getAttribute('href'),
    'https://psph.bitrix24.ru/crm/contact/details/27307/',
    'href — полный адрес: домена для перехода мало'
);
eq(lLink.getAttribute('target'), '_blank', 'открывается в новой вкладке');
eq(lLink.getAttribute('rel'), 'noopener noreferrer', 'без opener и без Referer');

// Превью ЗАПРОШЕНО — и у прокси, а не у VK.
showObserved();
await settle();
const lThumbCalls = netCalls.filter((c) => c.url.includes('/api/vk/media/1234/7045/'));
eq(lThumbCalls.length, 1, 'карточка ссылки запросила превью (раньше не запрашивала ВООБЩЕ)');
eq(
    lThumbCalls[0].url,
    '/api/vk/media/1234/7045/0?kind=thumb',
    'превью идёт через прокси бэкенда'
);
eq(lCard.querySelector('.vk-media-thumb').hidden, false, 'превью подставлено в карточку');
ok(
    String(lCard.querySelector('.vk-media-thumb').getAttribute('src')).startsWith('blob:'),
    'в <img src> блоб, а не адрес VK'
);
eq(lCard.querySelector('.vk-media-icon').hidden, true, 'значок-заглушка убран');

// OPEN ведёт на ссылку; SAVE по-прежнему нечего сохранять.
const lButtons = lCard.querySelectorAll('.vk-media-btn');
eq(lButtons[0].disabled, false, 'OPEN у ссылки работает');
eq(lButtons[1].disabled, true, 'SAVE у ссылки отключён — файла не бывает');
dispatch(lButtons[0], 'click', { target: lButtons[0] });
await settle();
eq(opened.length, 1, 'OPEN открыл ссылку во внешней вкладке');
eq(opened[0].url, 'https://psph.bitrix24.ru/crm/contact/details/27307/', 'адрес собеседника');
eq(opened[0].features, 'noopener,noreferrer', 'без opener и без Referer');

// Схему выбирал собеседник — значит, её надо отбрасывать.
for (const hostile of ['javascript:alert(1)', 'data:text/html,<b>x', 'vk://vk.com/im', '//evil.com/x']) {
    const badMsg = message(7046, [att(0, 'other', {
        has_thumb: false,
        has_full: false,
        title: 'Кликни',
        link_url: hostile,
    })]);
    const bCard = cardsOf(M.renderAttachmentGroup(badMsg))[0];
    eq(bCard.querySelector('.vk-media-link').hidden, true, `${hostile}: подписи нет`);
    eq(bCard.querySelector('.vk-media-link').getAttribute('href'), null, `${hostile}: href пуст`);
    const bOpen = bCard.querySelectorAll('.vk-media-btn')[0];
    eq(bOpen.disabled, true, `${hostile}: открывать нечем`);
    dispatch(bOpen, 'click', { target: bOpen });
    await settle();
    eq(opened.length, 1, `${hostile} во внешнюю вкладку не уходит`);
    M.resetMediaCards();
}

// ---------------------------------------------------------------------------
// 7. Голосовое: волна по waveform, плеер по клику.
// ---------------------------------------------------------------------------

const voice = message(7050, [att(0, 'voice', {
    has_thumb: false,
    mime: 'audio/ogg',
    duration_secs: 7,
    waveform: [1, 5, 9, 3, 7, 2],
    width: null,
    height: null,
})]);
const voiceCard = cardsOf(M.renderAttachmentGroup(voice))[0];
const wave = voiceCard.querySelector('.vk-media-wave');
ok(wave !== null && wave.children.length === 6, 'волна нарисована ПО ОГИБАЮЩЕЙ из VK');
ok(voiceCard.textContent.includes('0:07'), `длительность на карточке: ${voiceCard.textContent}`);

// ---------------------------------------------------------------------------
// 8. Имя для SAVE санитизируется.
// ---------------------------------------------------------------------------

const nasty = message(7060, [att(0, 'document', {
    file_name: '../../.bashrc',
    mime: 'text/plain',
})]);
const nCard = cardsOf(M.renderAttachmentGroup(nasty))[0];
const anchors = [];
const realCreate = globalThis.document.createElement;
globalThis.document.createElement = (tag) => {
    const node = realCreate(tag);
    if (String(tag).toLowerCase() === 'a') anchors.push(node);
    return node;
};
dispatch(nCard.querySelectorAll('.vk-media-btn')[1], 'click', {
    target: nCard.querySelectorAll('.vk-media-btn')[1],
});
await settle();
globalThis.document.createElement = realCreate;
eq(anchors.length, 1, 'SAVE создал ссылку на скачивание');
eq(anchors[0].download, 'bashrc', 'имя санитизировано: ни пути, ни ведущих точек');
ok(String(anchors[0].href).startsWith('blob:'), 'скачивается blob, а не адрес VK');

// ---------------------------------------------------------------------------
// 9. НИ ОДНОГО ПРЯМОГО URL VK В DOM.
// ---------------------------------------------------------------------------

const leaky = message(7070, [
    att(0, 'photo', { file_name: 'https://sun9.userapi.com/impg/leak.jpg' }),
    att(1, 'video', { has_full: false, player_url: 'https://vk.com/video_ext.php?oid=9' }),
]);
const groupL = M.renderAttachmentGroup(leaky);
showObserved();
await settle();
for (const node of [...groupL.querySelectorAll('.vk-media-thumb'), ...groupL.querySelectorAll('img')]) {
    const src = String(node.getAttribute('src') || '');
    ok(
        src === '' || src.startsWith('blob:'),
        `в <img src> обязан быть blob или пусто, получено: ${src}`
    );
}
const mediaRequests = netCalls.filter((c) => c.url.includes('userapi.com') || c.url.includes('sun9'));
eq(mediaRequests.length, 0, 'браузер не ходит на серверы VK напрямую');
ok(
    netCalls.filter((c) => c.url.includes('/7070/')).every((c) => c.url.startsWith('/api/vk/media/')),
    'все запросы вложений идут в прокси бэкенда'
);

// ---------------------------------------------------------------------------
// 10. REVOKE ПРИ СМЕНЕ БЕСЕДЫ — по счётчику objectURL.
// ---------------------------------------------------------------------------

const createdBefore = objectUrls.created;
ok(objectUrls.live.size > 0, 'к этому моменту живые блобы есть');
const freed = M.revokeDialogMedia();
ok(freed > 0, `revokeDialogMedia отозвал ${freed} ссылок`);
eq(objectUrls.live.size, 0, 'НИ ОДНОГО живого objectURL не осталось');
eq(objectUrls.created, objectUrls.revoked, 'создано ровно столько же, сколько отозвано');
eq(S.vkState.mediaCache.size, 0, 'кэш очищен');
void createdBefore;

// Тот же инвариант — через ленту: смена беседы обязана освободить блобы.
M.resetMediaCards();
setNet(async (url) => {
    if (url.includes('/api/vk/media/')) return mediaRes([9, 9, 9], 'image/jpeg');
    if (url.includes('/messages')) {
        return jsonRes(200, {
            messages: [message(8001, [att(0, 'photo'), att(1, 'photo')])],
            has_more: false,
        });
    }
    return jsonRes(200, {});
});
C.initChat({ onRead: () => {}, presenceOf: () => ({ online: false }) });
await C.openDialog('1234', DIALOG);
await settle();
showObserved();
await settle();
ok(objectUrls.live.size >= 2, `лента загрузила превью: живых блобов ${objectUrls.live.size}`);

await C.openDialog('4321', { id: '4321', kind: 'user', title: 'Другой' });
await settle();
eq(objectUrls.live.size, 0, 'смена беседы освободила ВСЕ objectURL');
eq(objectUrls.created, objectUrls.revoked, 'счётчики сошлись');

// ---------------------------------------------------------------------------
// 11. dropMessageMedia: правка сообщения выбрасывает ВСЕ его вложения.
// ---------------------------------------------------------------------------

S.vkState.activeDialogId = '1234';
S.vkState.mediaCache.set(F.mediaKey('1234', 9001, 0, 'thumb'), 'blob:vk/keep-0');
S.vkState.mediaCache.set(F.mediaKey('1234', 9001, 5, 'full'), 'blob:vk/keep-5');
S.vkState.mediaCache.set(F.mediaKey('1234', 9002, 0, 'thumb'), 'blob:vk/other');
M.dropMessageMedia('1234', 9001);
eq(
    Array.from(S.vkState.mediaCache.keys()),
    ['1234/9002/0/thumb'],
    'выброшены ВСЕ индексы правленого сообщения и ни одного чужого'
);
S.vkState.mediaCache.clear();

// ---------------------------------------------------------------------------
// 12. Строка ленты содержит группу карточек.
// ---------------------------------------------------------------------------

setNet(async (url) => {
    if (url.includes('/api/vk/media/')) return mediaRes([7], 'image/jpeg');
    if (url.includes('/messages')) {
        return jsonRes(200, {
            messages: [message(8100, [att(0, 'photo'), att(1, 'photo'), att(2, 'photo')])],
            has_more: false,
        });
    }
    return jsonRes(200, {});
});
await C.openDialog('1234', DIALOG);
await settle();
const row = $log.children.find((n) => n.dataset && n.dataset.msgId === '8100');
ok(row !== undefined, 'строка сообщения нарисована');
const slot = row.querySelector('.vk-line-media');
ok(slot !== null && slot.hidden === false, 'контейнер вложений показан');
eq(cardsOf(slot).length, 3, 'в строке ленты три карточки');

M.teardownMedia();
eq(objectUrls.live.size, 0, 'teardown не оставляет живых блобов');

// ---------------------------------------------------------------------------
console.log(`\nvk_media: проверок ${checks}, провалов ${failures}`);
process.exit(failures === 0 ? 0 : 1);
