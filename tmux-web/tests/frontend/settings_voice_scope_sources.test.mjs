// tmux-web / тест источников данных для выбора бесед авторазбора
// (`js/settings/voice-scope-sources.js`).
//
// Здесь работают НАСТОЯЩИЕ модули панелей (`vk/api.js`, `telegram/api.js`,
// `bitrix/api.js`) поверх мока `fetch` — иначе проверка «список бесед берётся
// тем же адресом, что и в форме видимости» проверяла бы мок, а не код.
//
// Главное обещание: у VK запрос идёт ФОНОВЫМ приоритетом общего планировщика.
// Токен VK ограничен тремя запросами в секунду НА ВЕСЬ КЛИЕНТ, и открытая
// модалка не имеет права отодвигать запросы самой панели.
//
// Запуск: `node tmux-web/tests/frontend/settings_voice_scope_sources.test.mjs`.

globalThis.window = { location: { origin: 'http://localhost' } };

/** Что уехало в сеть. */
const requests = [];
globalThis.fetch = async (url) => {
    requests.push(String(url));
    return {
        ok: true,
        status: 200,
        headers: { get: () => 'application/json' },
        json: async () => ({
            dialogs: [
                { id: '2000000042', title: 'Беседа', muted: true },
                { id: '777', title: 'Человек' },
                { id: 'chat7', title: 'Задача' },
            ],
            folders: [{ id: 5, title: 'Работа', emoticon: '💼' }],
        }),
    };
};

const S = await import(
    new URL('../../static/js/settings/voice-scope-sources.js', import.meta.url).href
);
const { PRIORITY_BG, vkLimiter } = await import(
    new URL('../../static/js/vk/ratelimit.js', import.meta.url).href
);

let failures = 0;
let checks = 0;
function eq(actual, expected, label) {
    checks += 1;
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a !== e) {
        failures += 1;
        console.error(`[FAIL] ${label}\n       ожидалось: ${e}\n       получено:  ${a}`);
    } else {
        console.log(`[ok]   ${label}`);
    }
}
function ok(cond, label) {
    checks += 1;
    if (!cond) {
        failures += 1;
        console.error(`[FAIL] ${label}`);
    } else {
        console.log(`[ok]   ${label}`);
    }
}

// ── VK: фоновый приоритет и словарь ТИПОВ бесед ─────────────────────────────

{
    const priorities = [];
    const original = vkLimiter.acquire.bind(vkLimiter);
    vkLimiter.acquire = async (p) => { priorities.push(p); return original(p); };

    requests.length = 0;
    const peers = await S.VK_SCOPE_SOURCE.listScopePeers();
    vkLimiter.acquire = original;

    eq(priorities, [PRIORITY_BG], 'запрос списка бесед VK идёт ФОНОВЫМ приоритетом');
    eq(requests.length, 1, 'ровно один запрос за показ формы — поллинга нет');
    ok(requests[0].includes('/api/vk/dialogs'), 'адрес тот же, что у формы видимости');
    ok(requests[0].includes(`limit=${S.PEER_LIST_LIMIT}`), 'лимит списка передан');
    eq(peers.length, 3, 'беседы разобраны');
    eq(peers[0].id, '2000000042', 'id беседы — строкой');
    ok(String(peers[0].marks).includes('беседы'), 'тип беседы показан пометкой');
    ok(String(peers[0].marks).includes('без звука'), 'приглушённость показана');

    const groups = await S.VK_SCOPE_SOURCE.listScopeGroups();
    eq(groups.map((g) => g.key), ['user', 'chat', 'community'],
        'группы VK — ТИПЫ бесед, тот же словарь, что у видимости');
}

// ── Telegram: папки аккаунта, inbox первым и archive последним ──────────────

{
    requests.length = 0;
    const groups = await S.TELEGRAM_SCOPE_SOURCE.listScopeGroups();
    eq(groups.map((g) => g.key), ['inbox', 'folder:5', 'archive'],
        'группы Telegram — ПАПКИ, порядок как в форме видимости');
    ok(String(groups[1].label).includes('Работа'), 'название папки взято у аккаунта');

    const peers = await S.TELEGRAM_SCOPE_SOURCE.listScopePeers();
    eq(peers.length, 3, 'чаты разобраны');
    ok(requests.some((u) => u.includes('/api/telegram/dialogs')), 'адрес чатов — телеграмный');
}

// ── Битрикс24: виды диалогов ────────────────────────────────────────────────

{
    requests.length = 0;
    const groups = await S.B24_SCOPE_SOURCE.listScopeGroups();
    eq(groups.map((g) => g.key), ['user', 'chat', 'task'],
        'группы Битрикс24 — ВИДЫ диалогов, включая чаты задач');

    const peers = await S.B24_SCOPE_SOURCE.listScopePeers();
    eq(peers.length, 3, 'диалоги разобраны');
    ok(requests.some((u) => u.includes('/api/bitrix/dialogs')), 'адрес диалогов — портальный');
}

if (failures > 0) {
    console.error(`\n${failures} of ${checks} checks FAILED (frontend/settings_voice_scope_sources)`);
    process.exit(1);
}
console.log(`\n${checks}/${checks} checks passed (frontend/settings_voice_scope_sources)`);
