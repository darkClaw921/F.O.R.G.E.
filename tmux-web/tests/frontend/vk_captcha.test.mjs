// Регресс-тест оверлея captcha панели VK (js/vk/captcha.js).
//
// Грузит РЕАЛЬНЫЙ модуль поверх фейкового DOM (tests/frontend/vk_dom_stub.mjs,
// импортируется ПЕРВЫМ — иначе core/dom.js резолвит ref'ы в null).
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Captcha (код 14) на пользовательском токене
// возникает почти исключительно при массовых рассылках, то есть в разработке
// этот путь скорее всего ни разу не сработает вживую. Значит, единственное
// его покрытие — стенд, юнит-маппинг кода и вот этот файл. Проверяются четыре
// правила, каждое из которых стоило бы пользователю бесконечного цикла
// разгадывания картинок:
//
//   1. картинка грузится ЧЕРЕЗ ПРОКСИ `/api/vk/captcha/:sid`, а не по прямому
//      `captcha_img` (браузер на серверы VK не ходит);
//   2. повтор ровно ОДИН — второй captcha_required показывает новую картинку,
//      но автоповтор не зацикливается;
//   3. пока оверлей открыт, фоновые запросы заморожены (флаг isCaptchaOpen);
//   4. отмена резолвит null — вызывающий обязан сказать об этом, а не молчать.
//
// Запуск: node tmux-web/tests/frontend/vk_captcha.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import {
    $captcha, $captchaImg, $captchaKey, $captchaSubmit, $captchaCancel,
    $captchaError, $document, dispatch, flush,
} from './vk_dom_stub.mjs';

const C = await import(new URL('../../static/js/vk/captcha.js', import.meta.url).href);
const S = await import(new URL('../../static/js/vk/state.js', import.meta.url).href);

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

/** Ошибка ровно в той форме, в какой её собирает js/vk/api.js. */
function captchaError(sid, img) {
    const err = new Error('VK requires a captcha');
    err.status = 409;
    err.code = 'captcha_required';
    err.body = { error: { code: 'captcha_required', captcha_sid: sid, captcha_img: img } };
    err.captcha = { sid, img };
    return err;
}

C.initCaptcha();

// ---------------------------------------------------------------------------
// 1. Показ по ошибке и адрес картинки.
// ---------------------------------------------------------------------------

{
    const pending = C.requestCaptcha(captchaError('239123', 'https://api.vk.com/captcha.php?sid=239123'));

    eq($captcha.hidden, false, 'оверлей показан');
    eq($captchaImg.getAttribute('src'), '/api/vk/captcha/239123', 'картинка идёт через прокси бэкенда');
    ok(
        !String($captchaImg.getAttribute('src')).includes('vk.com'),
        'в src нет прямого адреса VK — браузер на его серверы не ходит',
    );
    eq(C.isCaptchaOpen(), true, 'флаг заморозки фоновых запросов поднят');
    eq(S.vkState.captcha && S.vkState.captcha.sid, '239123', 'sid виден в состоянии панели');
    ok($captchaKey.focusCount > 0, 'фокус ушёл в поле ввода');
    eq(C._debugCaptcha().shown, 1, 'оверлей открыт один раз');

    // Пустой ввод не закрывает оверлей — иначе запрос ушёл бы без ответа.
    $captchaKey.value = '   ';
    dispatch($captchaSubmit, 'click', {});
    eq(C.isCaptchaOpen(), true, 'пустой ввод оверлей не закрывает');
    eq($captchaError.hidden, false, 'и объясняет, чего от пользователя ждут');

    $captchaKey.value = ' abcde ';
    dispatch($captchaSubmit, 'click', {});
    const answer = await pending;
    eq(answer, { sid: '239123', key: 'abcde' }, 'ответ содержит sid и введённые символы');
    eq($captcha.hidden, true, 'оверлей закрыт');
    eq(C.isCaptchaOpen(), false, 'заморозка снята');
    eq($captchaKey.value, '', 'поле очищено');
    eq($captchaImg.getAttribute('src'), null, 'src снят: картинка одноразовая');
    eq(S.vkState.captcha, null, 'состояние очищено');
}

// ---------------------------------------------------------------------------
// 2. Отмена: Esc, CANCEL и клик по подложке — все резолвят null.
// ---------------------------------------------------------------------------

{
    const byEsc = C.requestCaptcha({ sid: 'esc-1' });
    dispatch($document, 'keydown', { key: 'Escape' });
    eq(await byEsc, null, 'Esc резолвит null (вызывающий покажет системную строку)');
    eq(C.isCaptchaOpen(), false, 'после Esc заморозка снята');

    const byCancel = C.requestCaptcha({ sid: 'cancel-1' });
    dispatch($captchaCancel, 'click', {});
    eq(await byCancel, null, 'CANCEL резолвит null');

    const byBackdrop = C.requestCaptcha({ sid: 'backdrop-1' });
    dispatch($captcha, 'click', { target: $captcha });
    eq(await byBackdrop, null, 'клик по подложке резолвит null');

    // Клик ВНУТРИ коробки оверлей не закрывает: иначе попытка выделить текст
    // отменяла бы задачу.
    const inner = C.requestCaptcha({ sid: 'inner-1' });
    dispatch($captcha, 'click', { target: $captchaKey });
    eq(C.isCaptchaOpen(), true, 'клик по содержимому не закрывает оверлей');
    dispatch($captchaCancel, 'click', {});
    eq(await inner, null, 'и закрывается штатно');
}

// Esc при ЗАКРЫТОМ оверлее не должен ни бросать, ни перехватывать событие:
// иначе он отбирал бы Escape у лайтбокса и у поля ввода.
{
    const ev = dispatch($document, 'keydown', { key: 'Escape' });
    eq(ev.defaultPrevented, false, 'закрытый оверлей Escape не забирает');
    eq(ev.propagationStopped, false, 'и не гасит всплытие');
}

// Повторное закрытие не резолвит второй раз и не бросает.
{
    const p = C.requestCaptcha({ sid: 'double-close' });
    dispatch($captchaCancel, 'click', {});
    dispatch($captchaCancel, 'click', {});
    dispatch($document, 'keydown', { key: 'Escape' });
    eq(await p, null, 'двойное закрытие безопасно');
    eq(C.isCaptchaOpen(), false, 'состояние согласовано');
}

// Пустой источник — не повод открывать пустой оверлей.
{
    eq(await C.requestCaptcha({}), null, 'без sid оверлей не открывается');
    eq(await C.requestCaptcha(null), null, 'null-источник безопасен');
    eq($captcha.hidden, true, 'оверлей остался скрыт');
}

// Повторный запрос с ТЕМ ЖЕ sid отдаёт то же ожидание: `login_state` приходит
// и снапшотом REST, и кадром WS, и второй оверлей на ту же задачу был бы гонкой.
{
    const first = C.requestCaptcha({ sid: 'same-sid' });
    const shownBefore = C._debugCaptcha().shown;
    const second = C.requestCaptcha({ sid: 'same-sid' });
    eq(C._debugCaptcha().shown, shownBefore, 'тот же sid второй раз не показывается');
    ok(first === second, 'возвращается тот же промис');
    dispatch($captchaCancel, 'click', {});
    eq(await first, null, 'закрытие резолвит оба обращения');
}

// Новый sid ЗАМЕНЯЕТ задачу, а прошлое ожидание честно резолвится null.
{
    const stale = C.requestCaptcha({ sid: 'old-sid' });
    const fresh = C.requestCaptcha({ sid: 'new-sid' });
    eq(await stale, null, 'протухшая задача резолвится null, а не висит вечно');
    eq($captchaImg.getAttribute('src'), '/api/vk/captcha/new-sid', 'показана новая картинка');
    dispatch($captchaCancel, 'click', {});
    eq(await fresh, null, 'новая задача закрывается штатно');
}

// ---------------------------------------------------------------------------
// 3. withCaptcha: ровно один повтор.
// ---------------------------------------------------------------------------

// Успех с первого раза — оверлей не показывается вовсе.
{
    const shownBefore = C._debugCaptcha().shown;
    const calls = [];
    const res = await C.withCaptcha(async (extra) => {
        calls.push(extra);
        return 'ok';
    });
    eq(res, 'ok', 'результат проходит насквозь');
    eq(calls, [{}], 'первый вызов идёт без captcha-полей');
    eq(C._debugCaptcha().shown, shownBefore, 'оверлей не показан');
}

// Один captcha_required → показ → ответ → ОДИН повтор с полями.
{
    const calls = [];
    const promise = C.withCaptcha(async (extra) => {
        calls.push(extra);
        if (calls.length === 1) throw captchaError('sid-1', 'https://api.vk.com/captcha.php?sid=sid-1');
        return 'sent';
    });
    await flush();
    eq(C.isCaptchaOpen(), true, 'оверлей открыт после первого отказа');
    $captchaKey.value = 'zzz';
    dispatch($captchaSubmit, 'click', {});

    eq(await promise, 'sent', 'повтор вернул результат');
    eq(calls.length, 2, 'ровно один повтор');
    eq(calls[1], { captchaSid: 'sid-1', captchaKey: 'zzz' }, 'повтор = тот же запрос плюс два поля');
}

// Отмена → бросается ИСХОДНАЯ ошибка: вызывающий покажет системную строку.
{
    const calls = [];
    const promise = C.withCaptcha(async (extra) => {
        calls.push(extra);
        throw captchaError('sid-2', 'https://api.vk.com/captcha.php?sid=sid-2');
    });
    await flush();
    dispatch($captchaCancel, 'click', {});

    let caught = null;
    try { await promise; } catch (e) { caught = e; }
    ok(caught !== null, 'отмена не проглатывается молча');
    eq(caught && caught.code, 'captcha_required', 'наверх уходит исходный код');
    eq(calls.length, 1, 'после отмены повтора нет');
}

// Второй captcha_required подряд: новая картинка показана, автоповтор НЕ
// зациклен — вызывающий получает ошибку.
{
    const calls = [];
    const promise = C.withCaptcha(async (extra) => {
        calls.push(extra);
        const sid = calls.length === 1 ? 'sid-3a' : 'sid-3b';
        throw captchaError(sid, `https://api.vk.com/captcha.php?sid=${sid}`);
    });
    await flush();
    eq($captchaImg.getAttribute('src'), '/api/vk/captcha/sid-3a', 'первая картинка');
    $captchaKey.value = 'first-try';
    dispatch($captchaSubmit, 'click', {});

    let caught = null;
    try { await promise; } catch (e) { caught = e; }
    await flush();

    eq(calls.length, 2, 'ровно два вызова: исходный и ОДИН повтор');
    eq(caught && caught.code, 'captcha_required', 'вторая captcha уходит наверх ошибкой');
    eq($captchaImg.getAttribute('src'), '/api/vk/captcha/sid-3b', 'но новая картинка показана');
    eq(C.isCaptchaOpen(), true, 'оверлей открыт: состояние не осталось протухшим');
    dispatch($captchaCancel, 'click', {});
}

// Ошибка НЕ про captcha проходит насквозь без оверлея.
{
    const shownBefore = C._debugCaptcha().shown;
    const boom = new Error('flood');
    boom.code = 'flood_wait';
    let caught = null;
    try {
        await C.withCaptcha(async () => { throw boom; });
    } catch (e) {
        caught = e;
    }
    ok(caught === boom, 'чужая ошибка проходит как есть');
    eq(C._debugCaptcha().shown, shownBefore, 'оверлей на неё не открывается');
}

// closeCaptcha закрывает извне (logout, teardown вкладки).
{
    const p = C.requestCaptcha({ sid: 'ext-close' });
    C.closeCaptcha();
    eq(await p, null, 'closeCaptcha резолвит null');
    eq($captcha.hidden, true, 'оверлей скрыт');
    C.closeCaptcha(); // повтор безопасен
}

// initCaptcha идемпотентна: второй вызов не должен удваивать слушателей
// (двойной cancel резолвил бы дважды — это уже проверено выше, здесь ловим
// само удвоение по счётчику показов).
{
    C.initCaptcha();
    C.initCaptcha();
    const p = C.requestCaptcha({ sid: 'idem' });
    dispatch($captchaCancel, 'click', {});
    eq(await p, null, 'повторный initCaptcha не ломает оверлей');
    eq(C._debugCaptcha().bound, true, 'флаг привязки выставлен один раз');
}

if (failures > 0) {
    console.error(`\n${failures} of ${checks} checks FAILED`);
    process.exit(1);
}
console.log(`OK — ${checks} checks passed`);
