// Регресс-тест экрана логина панели VK (js/vk/login.js).
//
// Грузит РЕАЛЬНЫЙ модуль поверх фейкового DOM (tests/frontend/vk_dom_stub.mjs,
// импортируется ПЕРВЫМ — иначе core/dom.js резолвит ref'ы в null) и стаба
// `fetch`.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Экран логина — единственное место панели, через
// которое проходит СЕКРЕТ, и единственное, где пользователь может застрять
// навсегда. Отсюда состав проверок:
//
//   1. все семь состояний рисуются и РАЗЛИЧИМЫ ПО ТЕКСТУ (перепутанные
//      invalid_token и no_messages_scope стоили бы человеку часа вставки
//      одного и того же рабочего токена по кругу);
//   2. после успеха значение `#vk-token` ПУСТО — секрет не задерживается в DOM;
//   3. на бэкенд уходит ВСЯ вставленная строка целиком: фронт не разбирает
//      токен вообще, второй парсер был бы вторым местом для ошибки;
//   4. `renderLoginState` идемпотентна, а `initLogin` не удваивает слушателей;
//   5. flood-лимит даёт обратный отсчёт и НЕ уходит в автоповтор.
//
// Запуск: node tmux-web/tests/frontend/vk_login.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import {
    $login, $token, $submit, $status, $error, $countdown, $hint,
    $captcha, $captchaImg, $captchaKey, $captchaSubmit, $captchaCancel,
    dispatch, flush, jsonRes, netCalls, setNet,
} from './vk_dom_stub.mjs';

const L = await import(new URL('../../static/js/vk/login.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);
const C = await import(new URL('../../static/js/vk/captcha.js', import.meta.url).href);

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

function dto(state, extra) {
    return {
        state,
        error: null,
        user: null,
        captcha_sid: null,
        captcha_img: null,
        ...(extra || {}),
    };
}

L.initLogin();
C.initCaptcha();

// ---------------------------------------------------------------------------
// 1. Семь состояний: экран, текст, различимость.
// ---------------------------------------------------------------------------

{
    L.renderLoginState(dto('not_configured'));
    eq($login.hidden, false, 'not_configured: оверлей показан');
    eq($error.hidden, true, 'not_configured: ошибки нет — человек ещё ничего не вставлял');
    eq($submit.disabled, false, 'not_configured: кнопка доступна');

    L.renderLoginState(dto('checking'));
    eq($login.hidden, false, 'checking: оверлей показан');
    eq($status.hidden, false, 'checking: строка «проверяем…»');
    eq($error.hidden, true, 'checking: ошибку НЕ рисуем — это не отказ');
    eq($submit.disabled, true, 'checking: ввод заблокирован');
    eq($token.disabled, true, 'checking: поле заблокировано');

    const texts = {};
    for (const state of ['disconnected', 'invalid_token', 'no_messages_scope']) {
        // Между состояниями возвращаемся в not_configured: ошибка рисуется
        // при СМЕНЕ состояния (иначе повторный снапшот затирал бы более
        // свежий текст шага).
        L.renderLoginState(dto('not_configured'));
        L.renderLoginState(dto(state));
        eq($login.hidden, false, `${state}: оверлей показан`);
        eq($status.hidden, true, `${state}: «проверяем…» снято`);
        eq($error.hidden, false, `${state}: ошибка показана`);
        eq($submit.disabled, false, `${state}: форма доступна для повтора`);
        texts[state] = $error.textContent;
    }

    ok(texts.invalid_token.includes('VK 5'), 'invalid_token называет код VK');
    ok(texts.no_messages_scope.includes('VK 15'), 'no_messages_scope называет код VK');
    ok(
        texts.no_messages_scope.includes('standalone'),
        'no_messages_scope объясняет ПОЧЕМУ права нет — иначе токен вставляют по кругу',
    );
    ok(texts.disconnected.includes('api.vk.com'), 'disconnected говорит про связь');
    const uniq = new Set(Object.values(texts));
    eq(uniq.size, 3, 'три состояния — три РАЗНЫХ текста');

    // Текст ошибки от сервера сильнее нашего дефолта: он точнее.
    L.renderLoginState(dto('not_configured'));
    L.renderLoginState(dto('invalid_token', { error: 'VK error 5: User authorization failed' }));
    ok($error.textContent.includes('User authorization failed'), 'текст сервера доезжает до экрана');
}

// authorized: оверлей скрыт, поле ПУСТО, пользователь запомнен.
{
    $token.value = 'https://oauth.vk.ru/blank.html#access_token=vk1.a.SECRET';
    L.renderLoginState(dto('authorized', {
        user: { id: '105431859', first_name: 'Игорь', last_name: 'Герасимов', screen_name: 'igor' },
    }));

    eq($login.hidden, true, 'authorized: оверлей скрыт');
    eq($token.value, '', 'authorized: СЕКРЕТ НЕ ЗАДЕРЖИВАЕТСЯ В DOM');
    eq(L._debugLogin().tokenInDom, false, 'authorized: и по снимку состояния тоже');
    eq(S.vkState.loginState, 'authorized', 'состояние панели обновлено');
    eq(S.vkState.selfUser && S.vkState.selfUser.id, '105431859', 'пользователь запомнен');
}

// Идемпотентность: повторный вызов с тем же dto ничего не ломает.
{
    L.renderLoginState(dto('not_configured'));
    $token.value = 'что-то введённое';
    L.renderLoginState(dto('not_configured'));
    L.renderLoginState(dto('not_configured'));
    eq($token.value, 'что-то введённое', 'повторный снапшот не стирает ввод пользователя');
    eq($login.hidden, false, 'и не прячет оверлей');
    $token.value = '';
}

// Мусор вместо dto не должен ронять панель.
{
    let threw = false;
    try {
        L.renderLoginState(null);
        L.renderLoginState('строка');
        L.renderLoginState({});
    } catch (_) {
        threw = true;
    }
    ok(!threw, 'битый dto не роняет отрисовку');
}

// ---------------------------------------------------------------------------
// 2. Подсказка приходит с бэкенда.
// ---------------------------------------------------------------------------

{
    $hint.textContent = 'дефолт из index.html';
    L.setLoginHint('Скопируйте адресную строку целиком');
    eq($hint.textContent, 'Скопируйте адресную строку целиком', 'подсказка сервера применена');

    L.setLoginHint('');
    L.setLoginHint(null);
    L.setLoginHint(undefined);
    eq(
        $hint.textContent,
        'Скопируйте адресную строку целиком',
        'пустая подсказка не затирает текст: старый сервер не должен оставлять форму без объяснений',
    );
}

// ---------------------------------------------------------------------------
// 3. Отправка: вся строка целиком, без разбора на фронте.
// ---------------------------------------------------------------------------

{
    netCalls.length = 0;
    const PASTED = 'https://oauth.vk.ru/blank.html#access_token=vk1.a.CANARY_0123456789&expires_in=0&user_id=1&email=a%40b.c';
    setNet(async () => jsonRes(200, dto('authorized', { user: { id: '1' } })));

    L.renderLoginState(dto('not_configured'));
    $token.value = PASTED;
    dispatch($submit, 'click', {});
    await flush();

    eq(netCalls.length, 1, 'ровно один запрос — автоповторов у логина нет');
    eq(netCalls[0].url, '/api/vk/login/token', 'путь из замороженного контракта');
    eq(netCalls[0].method, 'POST', 'метод');
    eq(
        netCalls[0].body.access_token,
        PASTED,
        'на сервер уходит ВСЯ строка: разбор живёт только на бэкенде',
    );
    eq($token.value, '', 'после успеха поле очищено');
    eq($login.hidden, true, 'оверлей скрыт');
}

// Пустое поле в not_configured — подсказка, а не запрос.
{
    netCalls.length = 0;
    L.renderLoginState(dto('not_configured'));
    $token.value = '   ';
    dispatch($submit, 'click', {});
    await flush();
    eq(netCalls.length, 0, 'пустой ввод в сеть не уходит');
    eq($error.hidden, false, 'а объясняет, что вставлять');
}

// Пустое поле в НАСТРОЕННОМ состоянии — это «повторить проверку»: вставлять
// заново пользователю нечего.
{
    netCalls.length = 0;
    setNet(async () => jsonRes(200, dto('disconnected')));
    L.renderLoginState(dto('disconnected'));
    $token.value = '';
    dispatch($submit, 'click', {});
    await flush();
    eq(netCalls.length, 1, 'запрос ушёл');
    eq(netCalls[0].url, '/api/vk/login/verify', 'повтор проверки, а не отправка пустого токена');
}

// Enter в поле = нажатие CONNECT.
{
    netCalls.length = 0;
    setNet(async () => jsonRes(200, dto('authorized', { user: { id: '2' } })));
    L.renderLoginState(dto('not_configured'));
    $token.value = 'vk1.a.ENTER_KEY_TOKEN';
    const ev = dispatch($token, 'keydown', { key: 'Enter' });
    await flush();
    eq(ev.defaultPrevented, true, 'Enter не отправляет форму браузером');
    eq(netCalls.length, 1, 'Enter отправляет шаг');
    eq(netCalls[0].body.access_token, 'vk1.a.ENTER_KEY_TOKEN', 'значение доехало');
}

// initLogin идемпотентна: второй вызов не удваивает слушателей (иначе один
// клик отправил бы ДВА запроса — то есть два токена подряд).
{
    L.initLogin();
    L.initLogin();
    netCalls.length = 0;
    setNet(async () => jsonRes(200, dto('authorized', { user: { id: '3' } })));
    L.renderLoginState(dto('not_configured'));
    $token.value = 'vk1.a.DOUBLE_LISTENER_CHECK';
    dispatch($submit, 'click', {});
    await flush();
    eq(netCalls.length, 1, 'один клик — один запрос');
}

// ---------------------------------------------------------------------------
// 4. Ошибки: коды, обратный отсчёт, отсутствие автоповтора.
// ---------------------------------------------------------------------------

{
    netCalls.length = 0;
    setNet(async () => jsonRes(400, {
        error: { code: 'invalid_input', message: 'the pasted address has no access_token part' },
    }));
    L.renderLoginState(dto('not_configured'));
    $token.value = 'просто мусор';
    dispatch($submit, 'click', {});
    await flush();

    eq(netCalls.length, 1, 'мусор отправляется ОДИН раз (решает сервер, не фронт)');
    eq($error.hidden, false, 'ошибка показана');
    ok($error.textContent.includes('access_token'), 'текст сервера доезжает');
    eq($token.value, 'просто мусор', 'ввод не стёрт — пользователь правит его, а не вставляет заново');
}

{
    netCalls.length = 0;
    setNet(async () => jsonRes(429, {
        error: { code: 'flood_wait', message: 'too many', retry_after_secs: 90 },
    }));
    L.renderLoginState(dto('not_configured'));
    $token.value = 'vk1.a.FLOOD_TOKEN';
    dispatch($submit, 'click', {});
    await flush();

    eq($countdown.hidden, false, 'обратный отсчёт показан');
    ok($countdown.textContent.includes('01:30'), `формат MM:SS: ${$countdown.textContent}`);
    eq($submit.disabled, true, 'кнопка заблокирована на время ожидания');
    eq(L._debugLogin().waitLeft, 90, 'остаток ожидания в состоянии');

    // Пока тикает отсчёт, повторный клик НЕ отправляет запрос: автоповтор
    // flood-лимита только продлевает блокировку.
    const before = netCalls.length;
    dispatch($submit, 'click', {});
    await flush();
    eq(netCalls.length, before, 'во время ожидания запросы не уходят');

    L.stopCountdown();
    eq($countdown.hidden, true, 'stopCountdown убирает строку');
    eq($submit.disabled, false, 'и разблокирует кнопку');
    eq(L._debugLogin().timerActive, false, 'таймер погашен');
}

eq(L.formatWait(0), '00:00', 'formatWait: ноль');
eq(L.formatWait(59), '00:59', 'formatWait: секунды');
eq(L.formatWait(90), '01:30', 'formatWait: минуты');
eq(L.formatWait(3600), '01:00:00', 'formatWait: часы (квота метода у VK — час)');
eq(L.formatWait(-5), '00:00', 'formatWait: отрицательное');
eq(L.formatWait('мусор'), '00:00', 'formatWait: не число');

// ---------------------------------------------------------------------------
// 5. Captcha: состояние captcha_required открывает ОТДЕЛЬНЫЙ оверлей.
// ---------------------------------------------------------------------------

{
    L.stopCountdown();
    netCalls.length = 0;
    setNet(async () => jsonRes(200, dto('authorized', { user: { id: '9' } })));

    L.renderLoginState(dto('not_configured'));
    L.renderLoginState(dto('captcha_required', {
        captcha_sid: '239123456',
        captcha_img: 'https://api.vk.com/captcha.php?sid=239123456',
    }));
    await flush();

    eq($login.hidden, false, 'captcha_required: экран логина остаётся на месте');
    eq($captcha.hidden, false, 'captcha_required: открыт ОТДЕЛЬНЫЙ оверлей');
    eq(
        $captchaImg.getAttribute('src'),
        '/api/vk/captcha/239123456',
        'картинка идёт через прокси, а не по прямому captcha_img',
    );

    $captchaKey.value = 'abcde';
    dispatch($captchaSubmit, 'click', {});
    await flush();

    eq(netCalls.length, 1, 'ответ ушёл одним запросом');
    eq(netCalls[0].url, '/api/vk/captcha', 'путь ответа на captcha');
    eq(netCalls[0].body, { sid: '239123456', key: 'abcde' }, 'sid и символы');
    eq($login.hidden, true, 'после успеха оверлей логина скрыт');
}

// Отмена captcha не оставляет пользователя в тишине.
{
    netCalls.length = 0;
    L.renderLoginState(dto('not_configured'));
    L.renderLoginState(dto('captcha_required', { captcha_sid: 'cancel-me' }));
    await flush();
    dispatch($captchaCancel, 'click', {});
    await flush();

    eq(netCalls.length, 0, 'после отмены запрос не уходит');
    eq($error.hidden, false, 'но экран объясняет, что произошло');
    ok($error.textContent.includes('отменена'), `текст: ${$error.textContent}`);
}

// ---------------------------------------------------------------------------
// 6. resetLogin: чистит секрет и гасит таймер.
// ---------------------------------------------------------------------------

{
    $token.value = 'vk1.a.LEFTOVER';
    L.startCountdown(30);
    L.resetLogin();
    eq($token.value, '', 'resetLogin стирает секрет из DOM');
    eq(L._debugLogin().timerActive, false, 'и гасит таймер (иначе он тикал бы в фоне навсегда)');
    eq(L._debugLogin().lastState, null, 'состояние забыто — следующий dto применится как новый');
    eq($error.hidden, true, 'строка ошибки убрана');
}

if (failures > 0) {
    console.error(`\n${failures} of ${checks} checks FAILED`);
    process.exit(1);
}
console.log(`OK — ${checks} checks passed`);
