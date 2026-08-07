// Регресс-тест экрана подключения панели «Битрикс24» (js/bitrix/login.js).
//
// Грузит РЕАЛЬНЫЙ модуль поверх фейкового DOM (tests/frontend/bitrix_dom_stub.mjs,
// импортируется ПЕРВЫМ — иначе core/dom.js резолвит ref'ы в null) и стаба
// `fetch`.
//
// ЗАЧЕМ ЭТОТ ТЕСТ СУЩЕСТВУЕТ. Экран подключения — единственное место панели,
// через которое проходит СЕКРЕТ (у Битрикс24 это ВЕСЬ URL вебхука), и
// единственное, где пользователь может застрять навсегда. Отсюда состав
// проверок:
//
//   1. все восемь состояний автомата рисуются и РАЗЛИЧИМЫ ПО ТЕКСТУ
//      (перепутанные invalid_credentials и scope_missing стоили бы человеку
//      получаса вставки одного и того же исправного вебхука по кругу);
//   2. после успеха значение `#b24-webhook` ПУСТО — секрет не задерживается в
//      DOM и не уходит ни в какой сторадж;
//   3. на бэкенд уходит ВСЯ вставленная строка целиком, полем `webhook`:
//      фронт не разбирает адрес вообще, разбор трёх форм живёт на сервере;
//   4. `renderLoginState` идемпотентна, а `initB24Login` не удваивает
//      слушателей при повторном вызове;
//   5. троттлинг портала (Р16) даёт обратный отсчёт и НЕ уходит в автоповтор.
//
// Запуск: node tmux-web/tests/frontend/bitrix_login.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

import {
    $countdown, $error, $hint, $login, $logoutBtn, $logoutNote, $status,
    $submit, $webhook, dispatch, flush, jsonRes, netCalls, setNet,
} from './bitrix_dom_stub.mjs';

const L = await import(new URL('../../static/js/bitrix/login.js', import.meta.url).href);

let failures = 0;
let checks = 0;

function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
        return;
    }
    console.log(`[ok]   ${label}`);
}

function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
        return;
    }
    console.log(`[ok]   ${label}`);
}

/** LoginStateDto ровно той формы, что отдаёт plugins/bitrix/src/login.rs. */
function dto(state, extra) {
    return Object.assign({ state, error: null, retry_after_secs: null, user: null }, extra || {});
}

/** Секрет, который «вставляет пользователь». */
const WEBHOOK = 'https://test-work-2.bitrix24.ru/rest/1/wqdzbfhmn83d23j4/';

L.initB24Login();

// ---------------------------------------------------------------------------
// 1. Восемь состояний: оверлей, блокировка формы и РАЗЛИЧИМЫЕ тексты.
// ---------------------------------------------------------------------------

{
    const texts = new Map();
    for (const state of [
        'not_configured', 'invalid_url', 'invalid_credentials',
        'scope_missing', 'throttled', 'disconnected',
    ]) {
        L.renderLoginState(dto(state));
        texts.set(state, $error.textContent);
    }

    ok($login.hidden === false, 'оверлей открыт во всех состояниях, кроме authorized');
    ok(texts.get('not_configured') === '', 'стартовое состояние показывается БЕЗ ошибки');

    const withText = [...texts.entries()].filter(([s]) => s !== 'not_configured');
    ok(withText.every(([, t]) => t.startsWith('>> ERR:')), 'каждое состояние отказа объяснено строкой');
    eq(
        new Set(withText.map(([, t]) => t)).size,
        withText.length,
        'тексты состояний РАЗЛИЧИМЫ (перепутанные отправили бы чинить не то)',
    );
    ok(
        texts.get('scope_missing').includes('im') && texts.get('scope_missing').includes('task'),
        'экран нехватки прав называет ОБА нужных права',
    );
}

// checking: форма заблокирована, ошибки НЕТ.
{
    L.renderLoginState(dto('checking'));
    ok($webhook.disabled && $submit.disabled, 'на время проверки форма заблокирована');
    eq($error.hidden, true, 'во время проверки ошибка не показывается');
    ok($status.textContent.includes('connecting'), 'состояние проверки объяснено словами');
}

// ---------------------------------------------------------------------------
// 2. Успех: оверлей закрыт, СЕКРЕТ НЕ ОСТАЛСЯ В DOM.
// ---------------------------------------------------------------------------

{
    $webhook.value = WEBHOOK;
    let authorizedCalls = 0;
    L.initB24Login({ onAuthorized: () => { authorizedCalls += 1; } });

    L.renderLoginState(dto('authorized', { user: { id: '1', name: 'Игорь', last_name: 'Герасимов' } }));

    eq($login.hidden, true, 'после подключения оверлей скрыт');
    eq($webhook.value, '', 'СЕКРЕТ НЕ ЗАДЕРЖИВАЕТСЯ В DOM: поле очищено сразу после успеха');
    eq($logoutBtn.hidden, false, 'кнопка отвязки появляется только в authorized');
    eq(authorizedCalls, 1, 'колбэк onAuthorized вызван ровно один раз');

    // Идемпотентность: повторный тот же dto ничего не ломает и не зовёт
    // колбэк второй раз (снапшот `/status` и кадр WS приходят оба).
    L.renderLoginState(dto('authorized', { user: { id: '1' } }));
    eq(authorizedCalls, 1, 'повторный снапшот не вызывает onAuthorized заново');
    eq($login.hidden, true, 'повторный рендер не открывает оверлей обратно');
}

// ---------------------------------------------------------------------------
// 3. Отправка: на сервер уходит ВСЯ строка целиком, полем `webhook`.
// ---------------------------------------------------------------------------

{
    netCalls.length = 0;
    setNet(async () => jsonRes(200, dto('authorized', { user: { id: '1' } })));

    $webhook.value = `  ${WEBHOOK}  `;
    $submit.click();
    await flush();

    eq(netCalls.length, 1, 'нажатие CONNECT отправляет ровно один запрос');
    eq(netCalls[0].url, '/api/bitrix/login/webhook', 'путь совпадает с таблицей routes/mod.rs');
    eq(netCalls[0].method, 'POST', 'секрет уходит телом POST, а не query-строкой');
    eq(
        Object.keys(netCalls[0].body),
        ['webhook'],
        'тело содержит РОВНО поле webhook (имя — контракт с WebhookReq)',
    );
    eq(
        netCalls[0].body.webhook,
        `  ${WEBHOOK}  `,
        'фронт НЕ разбирает и не подчищает адрес: три формы разбирает сервер',
    );
    eq($webhook.value, '', 'после успеха поле снова пусто');
}

// Enter в поле работает так же, как кнопка.
{
    netCalls.length = 0;
    $webhook.value = WEBHOOK;
    const e = dispatch($webhook, 'keydown', { key: 'Enter' });
    await flush();
    eq(netCalls.length, 1, 'Enter в поле отправляет запрос');
    ok(e.defaultPrevented, 'Enter не отправляет форму браузером');
}

// Пустое поле — подсказка, а не запрос.
{
    netCalls.length = 0;
    $webhook.value = '   ';
    $submit.click();
    await flush();
    eq(netCalls.length, 0, 'пустое поле не порождает запроса');
    ok($error.textContent.includes('вставьте адрес'), 'пустое поле объяснено подсказкой');
}

// ---------------------------------------------------------------------------
// 4. Ошибки сервера разбираются ПО КОДУ, а не по тексту.
// ---------------------------------------------------------------------------

{
    const cases = [
        ['invalid_input', 400, 'это не адрес входящего вебхука'],
        ['scope_missing', 409, 'у вебхука нет прав: task'],
        ['not_authorized', 409, 'портал отверг вебхук'],
        ['not_connected', 503, null],
    ];
    for (const [code, status, message] of cases) {
        netCalls.length = 0;
        setNet(async () => jsonRes(status, { error: { code, message: message || 'boom' } }));
        $webhook.value = WEBHOOK;
        $submit.click();
        // eslint-disable-next-line no-await-in-loop
        await flush();
        ok($error.textContent.startsWith('>> ERR:'), `код ${code} объяснён пользователю`);
        ok(!$submit.disabled, `после отказа (${code}) кнопка снова активна`);
        ok(
            $webhook.value !== '',
            `после отказа (${code}) вставленное значение остаётся — иначе повтор = сходить в портал заново`,
        );
    }
}

// ---------------------------------------------------------------------------
// 5. Троттлинг (Р16): обратный отсчёт и ЗАПРЕТ автоповтора.
// ---------------------------------------------------------------------------

{
    netCalls.length = 0;
    setNet(async () => jsonRes(429, {
        error: { code: 'flood_wait', message: 'portal is throttling', retry_after_secs: 42 },
    }));
    $webhook.value = WEBHOOK;
    $submit.click();
    await flush();

    eq($countdown.hidden, false, 'обратный отсчёт показан');
    ok($countdown.textContent.includes('00:42'), 'остаток форматируется MM:SS');
    ok($submit.disabled, 'пока идёт отсчёт, кнопка заблокирована');

    // Автоповтора нет: повторный клик не уходит в сеть.
    const before = netCalls.length;
    $submit.click();
    await flush();
    eq(netCalls.length, before, 'АВТОПОВТОРА НЕТ: троттлинг лечится ожиданием, а не повтором');

    L.stopCountdown();
    eq($countdown.hidden, true, 'отсчёт останавливается');
    ok(!$submit.disabled, 'после отсчёта кнопка возвращается в строй');
}

// Снапшот `throttled` из /status тоже запускает отсчёт — и не перезапускает
// его при повторе того же состояния.
{
    L.renderLoginState(dto('not_configured'));
    L.renderLoginState(dto('throttled', { retry_after_secs: 7 }));
    ok($countdown.textContent.includes('00:07'), 'снапшот throttled запускает отсчёт');
    L.renderLoginState(dto('throttled', { retry_after_secs: 7 }));
    ok($countdown.textContent.includes('00:07'), 'повторный снапшот не откатывает таймер');
    L.stopCountdown();
}

// ---------------------------------------------------------------------------
// 6. Идемпотентность init и подсказки с бэкенда.
// ---------------------------------------------------------------------------

{
    const before = $submit.listenerCount('click');
    L.initB24Login();
    L.initB24Login({ onAuthorized: () => {} });
    eq($submit.listenerCount('click'), before, 'повторный initB24Login НЕ удваивает слушателей');
    eq($webhook.listenerCount('keydown'), 1, 'слушатель поля навешен ровно один раз');
    eq($logoutBtn.listenerCount('click'), 1, 'кнопка отвязки навешена ровно один раз');
}

{
    L.setLoginHint('  Разработчикам → Другое → Входящий вебхук  ');
    eq($hint.textContent, 'Разработчикам → Другое → Входящий вебхук', 'подсказка приходит с бэкенда');
    L.setLoginHint('');
    eq(
        $hint.textContent,
        'Разработчикам → Другое → Входящий вебхук',
        'пустое значение НЕ затирает подсказку: старый сервер не должен оставлять форму без объяснений',
    );

    L.setLogoutHint('На портале вебхук продолжит действовать.');
    eq(
        $logoutNote.textContent,
        'На портале вебхук продолжит действовать.',
        'предупреждение про выход тоже приходит с бэкенда',
    );
}

// ---------------------------------------------------------------------------
// 7. Выход: один запрос, колбэк и возврат оверлея.
// ---------------------------------------------------------------------------

{
    L.renderLoginState(dto('authorized', { user: { id: '1' } }));
    netCalls.length = 0;
    let loggedOut = 0;
    L.initB24Login({ onLoggedOut: () => { loggedOut += 1; } });
    setNet(async () => jsonRes(200, dto('not_configured')));

    $logoutBtn.click();
    await flush();

    eq(netCalls.length, 1, 'выход — ровно один запрос');
    eq(netCalls[0].url, '/api/bitrix/logout', 'путь выхода совпадает с таблицей routes/mod.rs');
    eq($login.hidden, false, 'после выхода оверлей снова открыт');
    eq($logoutBtn.hidden, true, 'кнопка отвязки спрятана');
    eq(loggedOut, 1, 'колбэк onLoggedOut вызван');
}

// ---------------------------------------------------------------------------
// 8. Снимок для e2e не содержит секрета — только факт его наличия.
// ---------------------------------------------------------------------------

{
    $webhook.value = WEBHOOK;
    const snap = JSON.stringify(L._debugLogin());
    ok(!snap.includes('wqdzbfhmn83d23j4'), '_debugLogin() не раскрывает код вебхука');
    ok(snap.includes('"webhookInDom":true'), '_debugLogin() сообщает ФАКТ наличия значения');
    L.resetLogin();
    eq($webhook.value, '', 'resetLogin чистит поле');
}

console.log(`\n${checks - failures}/${checks} checks passed`);
if (failures > 0) process.exit(1);
