// ⚠ ЖИВАЯ ПРОБА КАРТОЧКИ РАСШИФРОВКИ В НАСТОЯЩЕМ БРАУЗЕРЕ (Ф5 фичи голосовых).
//
// Соседние тесты работают на фейковом документе — и этого хватает ровно до тех
// пор, пока проверяемое не зависит от НАСТОЯЩЕГО браузера. А зависит здесь
// многое:
//
//   • `root.isConnected`. Панель зовёт `attach` при СБОРКЕ карточки, а в
//     документ карточка попадает позже — то есть свежий блок несколько
//     миллисекунд отцеплен. Первая версия модуля выбрасывала такие
//     представления как мусор, и расшифровка на экране не появлялась НИКОГДА:
//     кадры приходили в выброшенный узел. На фейковом документе дефекта не
//     видно вовсе — там нет `isConnected`. Поймано этой пробой;
//   • `-webkit-line-clamp`. «Свёрнут» — это не класс, это реально обрезанный
//     текст: `clientHeight < scrollHeight`. Проверить это без раскладки нельзя;
//   • ширина полосы прогресса в процентах от контейнера;
//   • `[hidden]` против собственного `display` узла;
//   • фокус на кнопках (клавиатурная навигация);
//   • КОНТРАСТ подписей в тёмной и светлой теме — по вычисленным цветам, а не
//     по именам токенов.
//
// Проба сама поднимает статический сервер на свободном порту и запускает
// Chrome for Testing из кэша Playwright в headless. Нет браузера — ПРОПУСК с
// сообщением, а не падение: обязательные проверки живут в соседних файлах.
//
// Запуск: node tmux-web/tests/frontend/voice_transcript_live.test.mjs
// Код возврата 0 — проверки прошли (или браузер не найден), 1 — есть [FAIL].

import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { extname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = fileURLToPath(new URL('.', import.meta.url));
const WEB_ROOT = fileURLToPath(new URL('../../', import.meta.url));
const HARNESS = join(HERE, 'voice_transcript_live.html');

/**
 * Chrome for Testing из кэша Playwright — самая свежая из установленных сборок.
 *
 * Тот же браузер, которым проверяются карточки встреч; своей копии Chrome
 * проект не тянет.
 */
function findChrome() {
    const fromEnv = process.env.CHROME_PATH;
    if (fromEnv && existsSync(fromEnv)) return fromEnv;
    const cache = join(process.env.HOME || '', 'Library/Caches/ms-playwright');
    if (!existsSync(cache)) return null;
    const builds = readdirSync(cache)
        .filter((n) => /^chromium-\d+$/.test(n))
        .sort((a, b) => Number(b.split('-')[1]) - Number(a.split('-')[1]));
    for (const build of builds) {
        for (const rel of [
            'chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
            'chrome-mac/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
            'chrome-linux/chrome',
        ]) {
            const p = join(cache, build, rel);
            if (existsSync(p)) return p;
        }
    }
    return null;
}

const chrome = findChrome();
if (!chrome) {
    console.log('[skip] voice_transcript_live: Chrome for Testing не найден (CHROME_PATH не задан)');
    process.exit(0);
}

const MIME = {
    '.html': 'text/html; charset=utf-8',
    '.js': 'text/javascript; charset=utf-8',
    '.mjs': 'text/javascript; charset=utf-8',
    '.css': 'text/css; charset=utf-8',
};

// Сервер отдаёт НАСТОЯЩИЕ static/js и static/css панели: проба обязана видеть
// ровно тот CSS, который увидит пользователь.
const server = createServer((req, res) => {
    const path = String(req.url || '').split('?')[0];
    if (path === '/' || path === '/probe.html') {
        res.writeHead(200, { 'content-type': MIME['.html'] });
        res.end(readFileSync(HARNESS));
        return;
    }
    const file = join(WEB_ROOT, path.replace(/^\/+/, ''));
    if (!file.startsWith(WEB_ROOT) || !existsSync(file)) {
        res.writeHead(404);
        res.end('нет');
        return;
    }
    res.writeHead(200, { 'content-type': MIME[extname(file)] || 'application/octet-stream' });
    res.end(readFileSync(file));
});

await new Promise((r) => server.listen(0, '127.0.0.1', r));
const port = server.address().port;

// ВАЖНО: браузер запускается АСИНХРОННО. `spawnSync` здесь не работает вовсе —
// он блокирует цикл событий Node, а значит поднятый выше сервер не примет ни
// одного соединения: Chrome молча упрётся в таймаут с пустым выводом.
const res = await new Promise((resolve) => {
    const child = spawn(chrome, [
        '--headless',
        '--disable-gpu',
        '--no-sandbox',
        // На macOS Chrome for Testing без этого лезет в связку ключей.
        '--use-mock-keychain',
        '--no-first-run',
        '--disable-extensions',
        '--disable-background-networking',
        // ⚠ БЕЗ `--user-data-dir`. С ним Chrome for Testing на macOS не
        // отдаёт `--dump-dom` вовсе: процесс висит до таймаута с пустым
        // выводом. Профиль по умолчанию (временный у headless) устраивает.
        // Ждём, пока модуль отработает: своей синхронизации у `--dump-dom` нет.
        '--virtual-time-budget=5000',
        '--dump-dom',
        `http://127.0.0.1:${port}/probe.html`,
    ], { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (c) => { stdout += c; });
    child.stderr.on('data', (c) => { stderr += c; });
    const timer = setTimeout(() => child.kill('SIGKILL'), 60000);
    child.on('close', () => { clearTimeout(timer); resolve({ stdout, stderr }); });
});
server.close();

const dom = res.stdout || '';
if (process.env.PROBE_DEBUG) console.error("DOMLEN", dom.length, "STDERR:", (res.stderr||"").split("\n").filter((l)=>!l.includes("ERROR:")).slice(0,20).join("\n"));
const body = /<pre id="out">([\s\S]*?)<\/pre>/.exec(dom);
if (!body) {
    console.error('[FAIL] voice_transcript_live: страница не отработала');
    console.error((res.stderr || '').split('\n').slice(0, 10).join('\n'));
    process.exit(1);
}
const text = body[1]
    .replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&').replace(/&quot;/g, '"');
const fails = text.split('\n').filter((l) => l.startsWith('[FAIL]'));
for (const l of fails) console.error(l);
const total = text.split('\n').filter((l) => /^\[(ok|FAIL)/.test(l)).length;
if (fails.length === 0) console.log(`[OK] voice_transcript_live: ${total} проверок в настоящем браузере`);
else console.error(`[FAIL] voice_transcript_live: ${fails.length} из ${total}`);
process.exit(fails.length ? 1 : 0);
