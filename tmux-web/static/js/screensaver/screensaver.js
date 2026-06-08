// tmux-web — Screensaver «Таверна дворфов» (ASCII-заставка)
//
// Полноэкранная вью #screensaver в #main, открывается кнопкой 🍺 в
// #settings-bar ВМЕСТО области сессий — по образцу «ежедневной сводки»
// (#daily-summary). Внутри — анимированная ASCII-сцена ИЗНУТРИ таверны:
// детализированный интерьер (стены, окна с луной, камин с живым пламенем,
// полки с бутылками, бочки, доска-меню, висячие фонари, кот у огня), за
// столами сидят дворфы, пьют, поднимают кружки и переговариваются репликами
// в облачках над головами (дев-юмор F.O.R.G.E.). Сцена кликабельна: клик по
// дворфу поднимает его кружку и выдаёт новую реплику; сосед иногда отвечает.
//
// РЕНДЕР — единый ASCII-«композитор»: вся сцена рисуется в одну моноширинную
// сетку (COLS×ROWS) в одном <pre id="ss-screen">. Слои: статичный интерьер
// (база) → анимированный ambient (пламя/свечи/дым) → бюсты дворфов. Единая
// сетка даёт идеальное выравнивание «за столами». Облачка-реплики (.ss-bubble)
// — отдельные DOM-элементы, позиционируются в пикселях по координатам голов
// (через измеренный размер ячейки). Клик мапится из пикселей в (row,col) и
// ищет дворфа по его bbox.
//
// Анимация — requestAnimationFrame с троттлингом (~8 fps). Полная остановка:
// hideScreensaver() отменяет rAF и listener'ы; self-guard (offsetParent===null);
// Esc; auto-пауза rAF в скрытой вкладке; prefers-reduced-motion замораживает
// кадры (реплики остаются). Внешние хуки гасят заставку: ws/attach.js (открытие
// сессии) и tabs/tabs.js::switchTab.
//
// XSS-безопасность: весь текст уходит через textContent.

import { showPlaceholder } from '../terminal/xterm.js';
import { fetchSessions } from '../sessions/sessions.js';
import {
    $screensaver, $screensaverBack, $ssStage, $home,
} from '../core/dom.js';

// ---------------------------------------------------------------------------
// Состояние модуля
// ---------------------------------------------------------------------------

let _built = false;
let _bound = false;
let _running = false;
let _rafId = null;
let _reduced = false;
let _tick = 0;            // счётчик кадров (для ambient-фаз)
let _lastRenderAt = 0;
let _lastFrameStr = '';   // последняя отрисованная строка (анти-дубль)

let _screenEl = null;     // <pre id="ss-screen">
let _bubbleLayer = null;  // контейнер облачек
let _base = null;         // базовая сетка интерьера (2D array of char)
let _dwarves = [];        // экземпляры дворфов
let _cellW = 8, _cellH = 14;

const RENDER_MS = 120;    // ~8 fps

// ---------------------------------------------------------------------------
// Утилиты
// ---------------------------------------------------------------------------

function perfNow() {
    return (typeof performance !== 'undefined' && performance.now)
        ? performance.now() : Date.now();
}
function rand(min, max) { return min + Math.random() * (max - min); }
function pick(arr) { return arr[(Math.random() * arr.length) | 0]; }

// ---------------------------------------------------------------------------
// Размер сцены (сетка)
// ---------------------------------------------------------------------------

const COLS = 100;
const ROWS = 34;

// ---------------------------------------------------------------------------
// Реплики
// ---------------------------------------------------------------------------

const LINES = [
    'Опять билд красный!',
    'git push --force… что может пойти не так?',
    'За зелёный CI! 🍺',
    'Кто сломал master?!',
    'Works on my machine.',
    'Ещё один rebase — и я домой.',
    'tmux спас мне сессию.',
    'Claude, допиши за меня тесты…',
    'Мерджим в пятницу — мы храбрые дворфы!',
    'Где мой коммит? Я точно его делал.',
    'Прод упал — наливай.',
    'Segfault… в Rust?! Невозможно.',
    'borrow checker снова победил.',
    'cargo build… пойду за элем.',
    'У меня лапки, ревьюйте сами.',
    'LGTM, не читал.',
    'Тесты флакают, как и я после третьей.',
    'Откатываемся! Откатываемся!',
    'Просто перезапусти pipeline.',
    'Это не баг, это фича таверны.',
    'Кто опять закоммитил .env?',
    'Зелёный билд — повод выпить.',
    'Красный билд — тоже повод выпить.',
    'Дедлайн был вчера, а эль — вечен.',
    'TODO: допить кружку.',
    'Я не пьян, я просто async.',
    'Конфликт слияния? Решу третьей кружкой.',
    'CI зелёный — чудо, выпьем!',
    'Кто-нибудь видел мой stash?',
    'Документация? Это устное предание.',
    'У нас не легаси, у нас рунические свитки.',
    'Деплой в пятницу 17:59 — погнали!',
    'Сначала эль, потом hotfix.',
    'Паника на проде, тост за храбрых!',
    'Опять 200 строк в одном коммите…',
    'Rebase или die.',
    'Кружка пуста — это P0!',
    'Я закрыл тикет. Закрыл и забыл.',
    'force-push в master — старая дворфская традиция.',
    'Линтер ругается, а я нет.',
    'Память течёт, эль — тоже.',
    'unwrap() и будь что будет!',
    'Где бочка с зависимостями?',
    'Сборка идёт 40 минут — успею кружку.',
    'Кто апрувнет мой PR за кружку?',
    'Тыкаем в прод — он живой!',
    'Мой код — поэзия, ревью не нужно.',
    'Грузим эль и артефакты.',
    'Я не баг чинил, я эль чинил.',
    'Hotfix на hotfix, и так до утра.',
    'За тех, кто дежурит на проде!',
    'Кэш протух, как и моя кружка.',
    'Один тест, одна кружка — баланс.',
    'Кто трогал прод руками?!',
    'Релиз готов… почти… ещё кружку.',
];

const RETORTS = [
    'Аргх, опять?!',
    'За это — наливай!',
    'Я предупреждал…',
    'Не я, клянусь киркой!',
    'Хех, классика.',
    'Звучит как пятница.',
    'Снова rollback…',
    'Да ладно?!',
    'Ставлю кружку, что снова я.',
    'Тост!',
    'Это знак — пора пить.',
    'Где Claude, когда он нужен?',
    'Бороду даю — так и было.',
    'Эль не врёт.',
    'Слыхали? Прод упал.',
    'За зелёный CI!',
    'Чокнемся!',
    'Я этого не видел.',
    'Молчу-молчу.',
    'Ещё по одной?',
    'Кирку на стол!',
    'Виноват компилятор.',
    'Это судьба, не баг.',
    'Передай бочонок.',
    'Ха! Ожидаемо.',
    'Налей и мне.',
    'Согласен на все сто.',
    'Да будет эль!',
];

// ---------------------------------------------------------------------------
// Сетка + примитивы рисования
// ---------------------------------------------------------------------------

function newGrid() {
    const g = new Array(ROWS);
    for (let r = 0; r < ROWS; r++) g[r] = new Array(COLS).fill(' ');
    return g;
}
function cloneGrid(src) {
    const g = new Array(ROWS);
    for (let r = 0; r < ROWS; r++) g[r] = src[r].slice();
    return g;
}
function setCh(g, r, c, ch) {
    if (r < 0 || r >= ROWS || c < 0 || c >= COLS) return;
    if (ch === ' ' || ch === undefined) return; // пробел = «прозрачно»
    g[r][c] = ch;
}
function setOpaque(g, r, c, ch) {
    if (r < 0 || r >= ROWS || c < 0 || c >= COLS) return;
    g[r][c] = ch;
}
function hline(g, r, c1, c2, ch) { for (let c = c1; c <= c2; c++) setOpaque(g, r, c, ch); }
function vline(g, c, r1, r2, ch) { for (let r = r1; r <= r2; r++) setOpaque(g, r, c, ch); }
function paintText(g, r, c, str) { for (let i = 0; i < str.length; i++) setCh(g, r, c + i, str[i]); }
function paintOpaque(g, r, c, str) { for (let i = 0; i < str.length; i++) setOpaque(g, r, c + i, str[i]); }
function blit(g, r, c, rows) { for (let i = 0; i < rows.length; i++) paintText(g, r + i, c, rows[i]); }
function box(g, r1, c1, r2, c2, st) {
    // st: [tl,tr,bl,br,h,v]
    hline(g, r1, c1 + 1, c2 - 1, st[4]);
    hline(g, r2, c1 + 1, c2 - 1, st[4]);
    vline(g, c1, r1 + 1, r2 - 1, st[5]);
    vline(g, c2, r1 + 1, r2 - 1, st[5]);
    setOpaque(g, r1, c1, st[0]); setOpaque(g, r1, c2, st[1]);
    setOpaque(g, r2, c1, st[2]); setOpaque(g, r2, c2, st[3]);
}
const DBL = ['╔', '╗', '╚', '╝', '═', '║'];
const SGL = ['┌', '┐', '└', '┘', '─', '│'];

// ---------------------------------------------------------------------------
// Декоративные спрайты интерьера
// ---------------------------------------------------------------------------

const SPR = {
    barrel: [
        ' .-=-. ',
        '|=====|',
        '|  o  |',
        '|=====|',
        " '---' ",
    ],
    cat: [
        ' /\\_/\\ ',
        '( =.= )',
        ' (")(")',
    ],
    lantern: [
        ' ╤ ',
        '[¤]',
    ],
    bottle: ['o', '8', '|'], // top, body, base (вертикально)
    moon: [
        ' _ ',
        '( `',
        ' ¯ ',
    ],
    stool: ['n'],
};

// Полка с бутылками: рисуем ряд бутылок и полку под ними.
function drawShelf(g, r, c1, c2) {
    for (let c = c1; c <= c2; c += 2) {
        setCh(g, r - 2, c, 'o');
        setCh(g, r - 1, c, '8');
    }
    hline(g, r, c1 - 1, c2 + 1, '━');
    setOpaque(g, r, c1 - 2, '┣'); setOpaque(g, r, c2 + 2, '┫');
}

// ---------------------------------------------------------------------------
// Построение статичного интерьера (база)
// ---------------------------------------------------------------------------

// Ячейки, которые анимируются (заполняются в renderAmbient).
let FIRE = null;     // {r,c,w}
let CANDLES = [];    // [{r,c}]
let SMOKE = null;    // {r,c}

function buildInterior() {
    const g = newGrid();
    FIRE = null; CANDLES = []; SMOKE = null;

    // Внешние стены таверны.
    box(g, 0, 0, ROWS - 1, COLS - 1, DBL);

    // Потолочные балки (узлы на линии балок; ряд вывески не трогаем).
    hline(g, 2, 1, COLS - 2, '═');
    for (const c of [17, 34, 50, 66, 83]) setOpaque(g, 2, c, '╦');

    // Вывеска.
    const sign = '≡══  THE BROKEN BUILD TAVERN — Эль, баги и легенды  ══≡';
    paintText(g, 1, ((COLS - sign.length) >> 1), sign);

    // Паутина в углах.
    paintText(g, 1, 1, '╲'); paintText(g, 2, 2, '╲'); paintText(g, 3, 1, '╲');
    paintText(g, 1, COLS - 2, '╱'); paintText(g, 2, COLS - 3, '╱'); paintText(g, 3, COLS - 2, '╱');

    // ----- Задняя стена: декор в рядах 3..15 (выше зоны дворфов 18..23) -----

    // Левое окно с луной.
    box(g, 3, 3, 8, 13, SGL);
    vline(g, 8, 4, 7, '│'); hline(g, 5, 4, 12, '─'); vline(g, 8, 4, 7, '│');
    blit(g, 4, 9, SPR.moon);

    // Доска-меню (левая стена, ниже окна).
    box(g, 9, 3, 15, 17, SGL);
    paintText(g, 10, 6, '~ МЕНЮ ~');
    paintText(g, 11, 5, 'Эль .... 2');
    paintText(g, 12, 5, 'Мёд .... 3');
    paintText(g, 13, 5, 'Рагу ... 4');
    paintText(g, 14, 5, 'Rollbk FREE');

    // Окна-фонари по бокам полок.
    box(g, 3, 20, 8, 28, SGL); vline(g, 24, 4, 7, '│'); hline(g, 5, 21, 27, '─');
    paintText(g, 4, 22, '* .'); paintText(g, 6, 25, '.*');
    box(g, 3, 71, 8, 79, SGL); vline(g, 75, 4, 7, '│'); hline(g, 5, 72, 78, '─');
    blit(g, 4, 73, SPR.moon);

    // Полки с бутылками (центр задней стены).
    drawShelf(g, 5, 33, 67);
    drawShelf(g, 8, 33, 67);
    paintText(g, 11, 40, 'm m m m m m m m m m'); // вязанки/гирлянда
    paintText(g, 12, 38, '«══════ ◇ BUILD GUILD ◇ ══════»');

    // Камин (правая стена) + кот у огня.
    box(g, 3, 84, 12, 97, SGL);
    hline(g, 11, 85, 96, '▀');           // под камина
    paintText(g, 10, 87, '▬▬▬  ▬▬▬');    // поленья
    FIRE = { r: 6, c: 87, w: 9 };        // зона пламени (анимация)
    blit(g, 12, 88, SPR.cat);            // кот у камина

    // Висячие фонари.
    for (const c of [30, 50, 70]) blit(g, 2, c, SPR.lantern);

    // Бочки штабелем у левой стены (дальний ряд + у пола) — далеко от дворфов.
    blit(g, 16, 2, SPR.barrel);
    blit(g, 27, 2, SPR.barrel);

    // Пол.
    hline(g, ROWS - 2, 1, COLS - 2, '─');
    for (let c = 4; c < COLS - 2; c += 8) setOpaque(g, ROWS - 2, c, '┬');

    return g;
}

// ---------------------------------------------------------------------------
// Столы (часть базы) + посадочные места
// ---------------------------------------------------------------------------

// Рисует стол с кружками/свечой/тарелками; возвращает кандл-позицию.
function drawTable(g, centerCol, topRow) {
    const half = 8;
    const c1 = centerCol - half, c2 = centerCol + half;
    paintOpaque(g, topRow, c1, '.' + '_'.repeat(c2 - c1 - 1) + '.');
    // Поверхность стола: кружки U, свеча (!), тарелки (o).
    const surf = '| U  o  ! o  U |';
    const sc = centerCol - ((surf.length) >> 1);
    paintOpaque(g, topRow + 1, c1, '|'); paintOpaque(g, topRow + 1, c2, '|');
    for (let c = c1 + 1; c < c2; c++) setOpaque(g, topRow + 1, c, ' ');
    paintText(g, topRow + 1, sc, surf.slice(1, -1));
    paintOpaque(g, topRow + 2, c1, "'" );
    hline(g, topRow + 2, c1 + 1, c2 - 1, '_');
    paintOpaque(g, topRow + 2, c2, "'");
    // Ножки.
    setOpaque(g, topRow + 3, c1 + 2, '|'); setOpaque(g, topRow + 3, c2 - 2, '|');
    // Свеча в центре (анимируемое пламя над '!').
    CANDLES.push({ r: topRow, c: centerCol });
    return { c1, c2 };
}

// ---------------------------------------------------------------------------
// Бюсты дворфов (детализированные, 2 кадра: idle / drink)
// ---------------------------------------------------------------------------

// Генератор бюста. helm0/helm1 — шлем (2 ряда, ~7 шир.), eyes/nose/beard — 5 шир.
// Кружка `▟`-стиль "U=" перемещается: idle (низ у стола) → drink (у лица).
function makeBust(helm0, helm1, eyes, nose, beard, acc) {
    const a = acc || '';
    const idle = [
        '  ' + helm0,
        ' ' + helm1,
        '|' + eyes + '|' + a,
        '|' + nose + '|',
        ' ' + beard,
        ' \\___/U=',
    ];
    const drink = [
        '  ' + helm0,
        ' ' + helm1 + ' _',
        '|' + eyes + '|U=',
        '|' + nose + '/',
        ' ' + beard,
        ' \\___/ ',
    ];
    return { idle, drink, w: 9, h: 6, headCol: 4 };
}

// 8 вариантов: разные шлемы, глаза, бороды, аксессуары (трубка, повязка…).
const BUSTS = [
    makeBust('\\Y/', '.{===}.', 'o   o', ' -=- ', '}WVWV{'),          // рогатый шлем
    makeBust(' ^ ',  '/=====\\', 'O   o', ' .v. ', '/MWMW\\', ',~'),  // остроконечный + трубка(,~)
    makeBust('___',  '(#####)', '-   -', ' >=< ', '\\~mm~/'),         // круглый шлем, прищур
    makeBust('<^>',  '[=====]', 'o   O', ' -o- ', '{~WW~}'),          // крылатый шлем
    makeBust('vYv',  '.-----.', '@   @', ' -=- ', ')VVVV('),          // налобная лента
    makeBust(' x ',  '/-----\\', 'o   o', ' .L. ', '<WMWM>', '"'),    // капюшон, шрам(")
    makeBust('/^\\', '|=====|', 'o   -', ' >=< ', '}}WW{{'),          // подмигивает (o -)
    makeBust('_П_',  '{=====}', 'O   O', ' -V- ', '/wwww\\'),         // корона-обод
];

// ---------------------------------------------------------------------------
// Размещение дворфов на сцене
// ---------------------------------------------------------------------------

function placeDwarves(g) {
    _dwarves = [];
    let bi = 0;
    const nextBust = () => BUSTS[(bi++) % BUSTS.length];

    // Дворфы раскиданы по таверне на разной «глубине» — НЕ одним рядом: кто-то
    // сидит парами за столами (вместе), кто-то поодиночке на табуретах в
    // разных уголках. Большие столы — только у пар (их немного, чтобы не
    // забить сцену); одиночки компактны (бюст + табурет), их легко разнести.
    //
    // Пары: tc=центр стола, tr=верхний ряд стола (≤28 — ножки tr+3 не задевают
    // пол), seatRow = tr-6. Бюст занимает seatRow..seatRow+5, стол ниже →
    // «сидит за столом». Координаты подобраны так, чтобы бюсты не пересекались
    // друг с другом и с декором стены (он заканчивается к ряду ~15).
    const GROUPS = [
        { tc: 20, tr: 22 },   // пара, у задней стены слева  (seatRow 16)
        { tc: 78, tr: 24 },   // пара, справа               (seatRow 18)
        { tc: 48, tr: 28 },   // пара, по центру впереди      (seatRow 22)
    ];
    for (const grp of GROUPS) {
        drawTable(_base, grp.tc, grp.tr);
        const seatRow = grp.tr - 6;
        for (const o of [-9, 1]) addDwarf(nextBust(), seatRow, grp.tc + o);
    }

    // Одиночки на табуретах (без большого стола) — в свободных колонках-
    // проходах, где за ними НЕТ чужих столов (иначе голова «ложится на стол»).
    // Разнесены по рядам и краям → сцена не выглядит одной шеренгой.
    const SOLO = [
        { r: 16, c: 88 },   // у камина, верх-право
        { r: 24, c: 88 },   // спереди справа (под тем, что у камина)
        { r: 15, c: 30 },   // у стойки, центр-зад
        { r: 22, c: 30 },   // центр-лево, ближе (под «барным»)
        { r: 24, c: 60 },   // центр-право, спереди (в проходе)
    ];
    for (const s of SOLO) {
        setOpaque(_base, s.r + 6, s.c + 4, 'n'); // табурет под дворфом
        addDwarf(nextBust(), s.r, s.c);
    }

    // Дым из трубки у дворфа-курильщика (бюст с acc=',~', индекс 1) над головой.
    if (_dwarves[1]) SMOKE = { r: _dwarves[1].r - 1, c: _dwarves[1].c + 8 };
}

function addDwarf(bust, r, c) {
    _dwarves.push({
        bust, r, c,
        w: bust.w, h: bust.h,
        headCol: c + bust.headCol,
        headRow: r,
        frame: 'idle',
        mugBoostUntil: 0,
        speakingUntil: 0,
        nextAutoAt: 0,
        forcedLine: null,
        bubble: null,
    });
}

// ---------------------------------------------------------------------------
// Ambient-анимация (пламя, свечи, дым) — рисуется поверх копии базы
// ---------------------------------------------------------------------------

const FIRE_ROWS = [
    [') (  ) ( ', ' ^( )^ ( ', '^^^^^^^^^'],
    ['( ) ( )( ', '( )^( )^  ', '^^^^^^^^^'],
    [' ) ( ) ( ', ' )^( )^(  ', '^^^^^^^^^'],
    ['( ) )( ( ', '^( )^( )  ', '^^^^^^^^^'],
];
const CANDLE_FLAMES = ['.', ',', '*', '˙'];
const SMOKE_FRAMES = ['°', '·', 'º', ' '];

function renderAmbient(g, t) {
    // Камин.
    if (FIRE) {
        const fr = _reduced ? FIRE_ROWS[0] : FIRE_ROWS[t % FIRE_ROWS.length];
        for (let i = 0; i < fr.length; i++) paintText(g, FIRE.r + i, FIRE.c, fr[i]);
    }
    // Свечи на столах.
    for (const cd of CANDLES) {
        const fl = _reduced ? '.' : CANDLE_FLAMES[(t + cd.c) % CANDLE_FLAMES.length];
        setOpaque(g, cd.r, cd.c, fl);
    }
    // Дым из трубки.
    if (SMOKE && !_reduced) {
        for (let i = 0; i < 3; i++) {
            const ch = SMOKE_FRAMES[(t + i) % SMOKE_FRAMES.length];
            setCh(g, SMOKE.r - i, SMOKE.c + (i % 2), ch);
        }
    }
}

// ---------------------------------------------------------------------------
// Композитор кадра
// ---------------------------------------------------------------------------

function renderFrame(now) {
    const g = cloneGrid(_base);
    renderAmbient(g, _tick);

    for (const d of _dwarves) {
        const drinking = now < d.mugBoostUntil;
        const rows = drinking ? d.bust.drink : d.bust.idle;
        blit(g, d.r, d.c, rows);
    }

    let s = '';
    for (let r = 0; r < ROWS; r++) s += g[r].join('') + '\n';
    if (s !== _lastFrameStr) {
        _screenEl.textContent = s;
        _lastFrameStr = s;
    }
}

// ---------------------------------------------------------------------------
// Реплики / облачка
// ---------------------------------------------------------------------------

function showBubble(d, text, now, dur) {
    if (!d.bubble) return;
    d.bubble.textContent = text;
    d.bubble.classList.add('show');
    d.speakingUntil = now + (dur || 3800);
    d.mugBoostUntil = now + 1200;
}
function hideBubble(d) {
    if (!d.bubble) return;
    d.bubble.classList.remove('show');
    d.speakingUntil = 0;
}

function autoSpeak(d, now) {
    const wasForced = !!d.forcedLine;
    const text = d.forcedLine || pick(LINES);
    d.forcedLine = null;
    showBubble(d, text, now);
    d.nextAutoAt = now + rand(6000, 13000);
    if (!wasForced && _dwarves.length > 1 && Math.random() < 0.4) {
        const other = pick(_dwarves.filter((x) => x !== d));
        other.forcedLine = pick(RETORTS);
        other.nextAutoAt = now + rand(1100, 2000);
    }
}

function onDwarfClick(d) {
    const now = perfNow();
    showBubble(d, pick(LINES), now, 4000);
    d.nextAutoAt = now + rand(6000, 11000);
    if (_dwarves.length > 1 && Math.random() < 0.55) {
        const other = pick(_dwarves.filter((x) => x !== d));
        other.forcedLine = pick(RETORTS);
        other.nextAutoAt = now + rand(900, 1700);
    }
}

// ---------------------------------------------------------------------------
// Подгонка размера + позиционирование облачек
// ---------------------------------------------------------------------------

function measureCell() {
    if (!_screenEl) return;
    const probe = document.createElement('span');
    const cs = getComputedStyle(_screenEl);
    probe.style.cssText = 'position:absolute;visibility:hidden;white-space:pre;';
    probe.style.fontFamily = cs.fontFamily;
    probe.style.fontSize = cs.fontSize;
    probe.style.lineHeight = cs.lineHeight;
    probe.textContent = 'M'.repeat(40);
    $ssStage.appendChild(probe);
    const rect = probe.getBoundingClientRect();
    _cellW = rect.width / 40;
    _cellH = parseFloat(cs.lineHeight) || rect.height;
    probe.remove();
}

function fitFont() {
    if (!_screenEl || !$ssStage) return;
    const sw = $ssStage.clientWidth || 800;
    const sh = $ssStage.clientHeight || 500;
    // Оценка: ширина ячейки ≈ 0.6·fs, высота строки ≈ 1.08·fs.
    let fs = Math.min(sw / (COLS * 0.6), sh / (ROWS * 1.08));
    fs = Math.max(6, Math.min(26, Math.floor(fs)));
    _screenEl.style.fontSize = fs + 'px';
    _screenEl.style.lineHeight = (fs * 1.08) + 'px';
    measureCell();
    // Коррекция, если по факту шире сцены.
    if (_cellW * COLS > sw) {
        fs = Math.max(6, Math.floor(fs * sw / (_cellW * COLS)));
        _screenEl.style.fontSize = fs + 'px';
        _screenEl.style.lineHeight = (fs * 1.08) + 'px';
        measureCell();
    }
}

function positionBubbles() {
    if (!_screenEl || !$ssStage) return;
    // Реальное положение <pre> с учётом transform: translate(-50%,-50%) —
    // через getBoundingClientRect относительно сцены (offsetLeft/Top врут).
    const pre = _screenEl.getBoundingClientRect();
    const st = $ssStage.getBoundingClientRect();
    const offL = pre.left - st.left;
    const offT = pre.top - st.top;
    for (const d of _dwarves) {
        if (!d.bubble) continue;
        d.bubble.style.left = (offL + (d.headCol + 0.5) * _cellW) + 'px';
        d.bubble.style.top = (offT + d.headRow * _cellH) + 'px';
    }
}

function _onResize() {
    fitFont();
    positionBubbles();
}

// ---------------------------------------------------------------------------
// Клик по дворфу (мапинг пиксель → ячейка → дворф)
// ---------------------------------------------------------------------------

function _onScreenClick(ev) {
    const rect = _screenEl.getBoundingClientRect();
    const col = Math.floor((ev.clientX - rect.left) / _cellW);
    const row = Math.floor((ev.clientY - rect.top) / _cellH);
    let hit = null;
    for (const d of _dwarves) {
        if (row >= d.r && row < d.r + d.h && col >= d.c && col < d.c + d.w) { hit = d; break; }
    }
    if (hit) onDwarfClick(hit);
}

// ---------------------------------------------------------------------------
// Listener'ы показа
// ---------------------------------------------------------------------------

function _onKeydown(ev) {
    if (ev.key === 'Escape') { ev.stopPropagation(); closeScreensaver(); }
}
function _onVisibility() {
    if (document.hidden) return;
    const now = perfNow();
    for (const d of _dwarves) if (d.nextAutoAt < now) d.nextAutoAt = now + rand(800, 2500);
    _lastRenderAt = 0;
}

// ---------------------------------------------------------------------------
// Построение DOM сцены (один раз)
// ---------------------------------------------------------------------------

function buildScene() {
    if (_built || !$ssStage) return;
    _built = true;
    $ssStage.innerHTML = '';

    _base = buildInterior();

    _screenEl = document.createElement('pre');
    _screenEl.className = 'ss-screen';
    _screenEl.id = 'ss-screen';
    $ssStage.appendChild(_screenEl);

    _bubbleLayer = document.createElement('div');
    _bubbleLayer.className = 'ss-bubbles';
    $ssStage.appendChild(_bubbleLayer);

    placeDwarves(_base);

    // Облачко на каждого дворфа.
    for (const d of _dwarves) {
        const b = document.createElement('div');
        b.className = 'ss-bubble';
        d.bubble = b;
        _bubbleLayer.appendChild(b);
    }

    _screenEl.addEventListener('click', _onScreenClick);
}

// ---------------------------------------------------------------------------
// Анимационный цикл
// ---------------------------------------------------------------------------

function loop() {
    if (!_running) return;
    if (!$screensaver || $screensaver.offsetParent === null) { stopLoop(); return; }
    const now = perfNow();

    if (now - _lastRenderAt >= (_reduced ? 600 : RENDER_MS)) {
        _lastRenderAt = now;
        _tick++;
        renderFrame(now);
    }

    for (const d of _dwarves) {
        if (d.speakingUntil && now >= d.speakingUntil) hideBubble(d);
        if (now >= d.nextAutoAt && now >= d.speakingUntil) autoSpeak(d, now);
    }

    _rafId = requestAnimationFrame(loop);
}

function startLoop() {
    if (_running) return;
    _running = true;
    const now = perfNow();
    _lastRenderAt = 0;
    _dwarves.forEach((d, i) => { d.nextAutoAt = now + 500 + i * rand(500, 1100); });
    _rafId = requestAnimationFrame(loop);
}
function stopLoop() {
    _running = false;
    if (_rafId != null) { cancelAnimationFrame(_rafId); _rafId = null; }
}

// ---------------------------------------------------------------------------
// Публичный API
// ---------------------------------------------------------------------------

function bindControls() {
    if (_bound) return;
    _bound = true;
    if ($screensaverBack) $screensaverBack.addEventListener('click', () => closeScreensaver());
}

export function showScreensaver() {
    if (!$screensaver) return;
    bindControls();
    buildScene();
    _reduced = !!(window.matchMedia
        && window.matchMedia('(prefers-reduced-motion: reduce)').matches);

    try {
        if (window.ForgeApp && typeof window.ForgeApp.hideDailySummary === 'function') {
            window.ForgeApp.hideDailySummary();
        }
    } catch (_) { /* no-op */ }

    $screensaver.style.display = 'flex';
    if ($home) $home.style.display = 'none';
    showPlaceholder(false);

    // Рисуем первый кадр сразу, чтобы <pre> получил реальный размер; затем
    // (когда контейнер виден) подгоняем шрифт и считаем координаты облачек.
    renderFrame(perfNow());
    requestAnimationFrame(() => { fitFont(); positionBubbles(); });

    document.addEventListener('keydown', _onKeydown, true);
    document.addEventListener('visibilitychange', _onVisibility);
    window.addEventListener('resize', _onResize);

    startLoop();
}

export function hideScreensaver() {
    stopLoop();
    document.removeEventListener('keydown', _onKeydown, true);
    document.removeEventListener('visibilitychange', _onVisibility);
    window.removeEventListener('resize', _onResize);
    for (const d of _dwarves) hideBubble(d);
    if ($screensaver) $screensaver.style.display = 'none';
}

export function closeScreensaver() {
    hideScreensaver();
    fetchSessions();
}
