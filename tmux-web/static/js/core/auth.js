// tmux-web — auth bootstrap + fetch override (Phase 0 ES Modules refactor)
//
// 1:1 копия auth-логики из IIFE `tmux-web/static/app.js` (строки 15-87):
//   - AUTH_TOKEN_KEY константа (app.js:23)
//   - bootstrapAuthToken() IIFE (app.js:24-47)
//   - getAuthToken()       (app.js:49-52)
//   - fetch override       (app.js:54-78)
//   - withWsToken(url)     (app.js:82-87)
//
// ВАЖНО — side-effect import:
// Этот модуль ВЫПОЛНЯЕТ при импорте две побочки на верхнем уровне:
//   1) bootstrapAuthToken() — читает token из location.hash и кладёт в
//      localStorage (чистит hash из URL).
//   2) подмена window.fetch — оборачивает в Authorization: Bearer ...
//
// В Phase 1 main.js должен импортировать этот модуль ПЕРВЫМ, ДО любого другого
// модуля, который может позвать fetch() (даже неявно). Иначе override
// произойдёт после первых запросов и они уйдут без токена.
//
// В Phase 0 модуль ещё НЕ подключен к index.html — legacy app.js по-прежнему
// выполняет ту же IIFE-логику; модуль готов к импорту из main.js в Phase 1.

// ---- Auth bootstrap (remote-mode + mobile QR) ----
//
// При входе по QR-ссылке вида http://host:port#token=<token> читаем
// токен из hash, сохраняем в localStorage и удаляем hash из URL,
// чтобы он не висел в адресной строке. Далее токен используется в:
//   - подменённом window.fetch → Authorization: Bearer <token>;
//   - withWsToken(url) → '?token=...' query для WebSocket (браузер
//     не даёт ставить custom headers на WS из JS).
const AUTH_TOKEN_KEY = 'forge.authToken';

(function bootstrapAuthToken() {
    try {
        const hash = location.hash || '';
        const m = hash.match(/[#&]token=([^&]+)/);
        if (m) {
            const token = decodeURIComponent(m[1]);
            if (token) {
                localStorage.setItem(AUTH_TOKEN_KEY, token);
                // Чистим hash, чтобы токен не висел в URL.
                try {
                    history.replaceState(
                        null,
                        '',
                        location.pathname + location.search,
                    );
                } catch (_) {
                    location.hash = '';
                }
            }
        }
    } catch (e) {
        console.warn('[auth] failed to parse token from hash', e);
    }
})();

export function getAuthToken() {
    try { return localStorage.getItem(AUTH_TOKEN_KEY) || ''; }
    catch (_) { return ''; }
}

// Подмена fetch: для всех same-origin запросов добавляем Bearer-токен,
// если он есть. Не трогаем external URLs (CDN xterm.js и т.п.).
const _origFetch = window.fetch.bind(window);
window.fetch = function (input, init) {
    const token = getAuthToken();
    if (!token) return _origFetch(input, init);
    // Определяем URL-объект чтобы проверить same-origin.
    let urlStr;
    if (typeof input === 'string') urlStr = input;
    else if (input && typeof input.url === 'string') urlStr = input.url;
    else return _origFetch(input, init);
    try {
        const u = new URL(urlStr, location.href);
        if (u.origin !== location.origin) return _origFetch(input, init);
    } catch (_) {
        // Не парсится — относительный URL, считаем same-origin.
    }
    const next = Object.assign({}, init || {});
    const h = new Headers((init && init.headers) || {});
    if (!h.has('Authorization')) {
        h.set('Authorization', 'Bearer ' + token);
    }
    next.headers = h;
    return _origFetch(input, next);
};

// Добавляет token-query к WS-URL. Если токена нет — возвращает URL без
// изменений (legacy localhost-mode).
//
// ПРИМЕЧАНИЕ по безопасности: токен идёт в query-string WS-URL. Это
// сознательное ограничение браузера: WebSocket API не позволяет задать
// произвольные заголовки (нет аналога Authorization для рукопожатия), поэтому
// query-параметр — единственный способ передать токен из браузера. Сервер
// принимает токен и из заголовка, и из query (см. auth middleware). Риск
// (попадание в access-логи прокси) принимается; на сервере доступ к WS-URL
// логируется без query-части там, где это контролируется нами.
export function withWsToken(wsUrl) {
    const token = getAuthToken();
    if (!token) return wsUrl;
    const sep = wsUrl.includes('?') ? '&' : '?';
    return wsUrl + sep + 'token=' + encodeURIComponent(token);
}
