// Тесты чтения встречи по пробам (`src/bot/inject/observe.js`).
//
// Проверяется то, ради чего пробы вообще сделаны данными: перебор кандидатов
// по порядку, устойчивость к битому селектору из пользовательского TOML и
// ПОЛНАЯ ДЕГРАДАЦИЯ при пустом наборе — снимок обязан получиться и в этом
// случае, просто пустой.
//
// DOM здесь свой, крошечный. Настоящий jsdom потребовал бы зависимости npm в
// проекте, где её нет, а нужен ровно тот кусок, который трогают пробы:
// `querySelectorAll`, `matches`, `getAttribute`, `textContent`.

import test from "node:test";
import assert from "node:assert/strict";

import {
    forgeVisible,
    forgeProbeAll,
    forgeProbeHit,
    forgeProbeFirst,
    forgeProbePresent,
    forgeMarked,
    forgeKeyAttr,
    forgeText,
    forgeReadParticipants,
    forgeReadActiveSpeaker,
    forgePostCallUrl,
    forgeObserveMeeting
} from "../../src/bot/inject/observe.js";

// --- крошечный DOM ----------------------------------------------------------

/** Разбирает простой селектор в набор условий. Бросает на битом — как браузер. */
function parseSimple(part) {
    const conds = [];
    let rest = part;
    while (rest.length) {
        // Универсальный селектор. Нужен `contains:`, который обходит ВСЕ узлы;
        // без этой ветки крошечный DOM бросал бы SyntaxError, проба молча
        // возвращала пустоту, и тест маркеров зеленел бы вхолостую.
        if (rest[0] === "*") {
            conds.push({ kind: "any" });
            rest = rest.slice(1);
        } else if (rest[0] === "[") {
            const end = rest.indexOf("]");
            if (end === -1) { throw new SyntaxError("незакрытая скобка: " + part); }
            const body = rest.slice(1, end);
            const m = body.match(/^([\w-]+)(?:([*^$]?)=(?:"([^"]*)"|'([^']*)'|([^\s\]]+))\s*(i)?)?$/);
            if (!m) { throw new SyntaxError("битый атрибут: " + body); }
            conds.push({ kind: "attr", name: m[1], op: m[2] || "=", value: m[3] ?? m[4] ?? m[5], ci: !!m[6] });
            rest = rest.slice(end + 1);
        } else if (rest[0] === ".") {
            const m = rest.match(/^\.([\w-]+)/);
            conds.push({ kind: "class", value: m[1] });
            rest = rest.slice(m[0].length);
        } else if (rest[0] === "#") {
            const m = rest.match(/^#([\w-]+)/);
            conds.push({ kind: "id", value: m[1] });
            rest = rest.slice(m[0].length);
        } else {
            const m = rest.match(/^[\w-]+/);
            if (!m) { throw new SyntaxError("битый селектор: " + part); }
            conds.push({ kind: "tag", value: m[0].toLowerCase() });
            rest = rest.slice(m[0].length);
        }
    }
    if (!conds.length) { throw new SyntaxError("пустой селектор"); }
    return conds;
}

class El {
    constructor(tag, attrs = {}, children = [], text = "") {
        this.tag = tag;
        this.attrs = attrs;
        this.children = children;
        this.ownText = text;
        for (const c of children) { c.parent = this; }
        this.parent = null;
    }

    get textContent() {
        return this.ownText + this.children.map((c) => c.textContent).join("");
    }

    get value() { return this.attrs.value; }

    getAttribute(name) {
        return Object.prototype.hasOwnProperty.call(this.attrs, name) ? this.attrs[name] : null;
    }

    matchesSimple(conds) {
        return conds.every((c) => {
            if (c.kind === "any") { return true; }
            if (c.kind === "tag") { return this.tag === c.value; }
            if (c.kind === "id") { return this.attrs.id === c.value; }
            if (c.kind === "class") {
                return String(this.attrs.class || "").split(/\s+/).includes(c.value);
            }
            const raw = this.getAttribute(c.name);
            if (raw === null) { return false; }
            const have = c.ci ? String(raw).toLowerCase() : String(raw);
            const want = c.ci ? String(c.value ?? "").toLowerCase() : String(c.value ?? "");
            if (c.value === undefined) { return true; }
            if (c.op === "*") { return have.includes(want); }
            if (c.op === "^") { return have.startsWith(want); }
            if (c.op === "$") { return have.endsWith(want); }
            return have === want;
        });
    }

    matches(selector) {
        return String(selector).split(",").some((part) => {
            const chunks = part.trim().split(/\s+/);
            return this.matchesSimple(parseSimple(chunks[chunks.length - 1]));
        });
    }

    descendants() {
        const out = [];
        for (const c of this.children) {
            out.push(c);
            out.push(...c.descendants());
        }
        return out;
    }

    querySelectorAll(selector) {
        const parts = String(selector).split(",").map((p) => p.trim()).filter(Boolean);
        if (!parts.length) { throw new SyntaxError("пустой селектор"); }
        const chains = parts.map((p) => p.split(/\s+/).map(parseSimple));
        const out = [];
        for (const node of this.descendants()) {
            const hit = chains.some((chain) => {
                if (!node.matchesSimple(chain[chain.length - 1])) { return false; }
                let up = node.parent;
                for (let i = chain.length - 2; i >= 0; i--) {
                    while (up && !up.matchesSimple(chain[i])) { up = up.parent; }
                    if (!up) { return false; }
                    up = up.parent;
                }
                return true;
            });
            if (hit) { out.push(node); }
        }
        return out;
    }

    querySelector(selector) {
        return this.querySelectorAll(selector)[0] || null;
    }
}

const el = (tag, attrs, children, text) => new El(tag, attrs, children, text);

/** Плитка участника в духе мок-встречи: маркер речи НА САМОЙ плитке. */
function mockTile(id, name, speaking) {
    return el("li", { "data-testid": "participant", "data-participant-id": id, "data-speaking": String(speaking) }, [
        el("span", { "data-testid": "participant-name" }, [], name)
    ]);
}

/** Плитка в духе Телемоста: маркер речи в ПОТОМКЕ, классы с хешами. */
function telemostTile(name, speaking) {
    const inner = el("div", { class: speaking ? "root_ypmDo rootStroke_ab12" : "root_ypmDo" }, [
        el("div", { class: "participantName_x9 TextName_BOaIg" }, [], name)
    ]);
    return el("div", { class: "item_NZ2DW newGridItem_QQ11" }, [inner]);
}

const MOCK_PROBES = {
    participant: ["[data-testid=\"participant\"]"],
    participant_name: ["[data-testid=\"participant-name\"]"],
    speaking_marker: ["[data-speaking=\"true\"]"],
    participant_key_attr: ["attr:data-participant-id"],
    active_speaker: ["[data-testid=\"active-speaker\"]"],
    mic_mute_button: ["[data-testid=\"mic-toggle\"][data-enabled=\"true\"]"],
    mic_unmute_button: ["[data-testid=\"mic-toggle\"][data-enabled=\"false\"]"],
    cam_disable_button: ["[data-testid=\"cam-toggle\"][data-enabled=\"true\"]"],
    cam_enable_button: ["[data-testid=\"cam-toggle\"][data-enabled=\"false\"]"],
    join_button: ["[data-testid=\"join\"]"],
    meeting_ended_marker: ["[data-testid=\"meeting-ended\"][data-ended=\"true\"]"],
    post_call_url: ["/after-call"]
};

/** Плитка LiveKit (vcc.airlabs.pro): имя на ВЛОЖЕННОМ узле, признаки атрибутами. */
function livekitTile(name, { local = false, speaking = false, source = "camera" } = {}) {
    return el("div", {
        class: "lk-participant-tile",
        "data-lk-local-participant": String(local),
        "data-lk-speaking": String(speaking),
        "data-lk-source": source
    }, [
        el("span", { "data-lk-participant-name": name }, [], name)
    ]);
}

/**
 * ПЛИТКА ДЕМОНСТРАЦИИ ЭКРАНА — не человек, но по классу неотличима от него.
 * Всё снято живой пробой: имя с суффиксом «'s screen», источник `screen_share`,
 * и индикатор речи встаёт на ней ОДНОВРЕМЕННО с плиткой камеры хозяина.
 */
function livekitScreenTile(owner, { speaking = false } = {}) {
    return livekitTile(`${owner}'s screen`, { speaking, source: "screen_share" });
}

const VCC_PROBES = {
    participant: [".lk-participant-tile[data-lk-source=\"camera\"]"],
    participant_name: ["[data-lk-participant-name]"],
    local_participant_marker: ["[data-lk-local-participant=\"true\"]"],
    speaking_marker: ["[data-lk-speaking=\"true\"]"],
    participant_key_attr: [],
    active_speaker: []
};

const TELEMOST_PROBES = {
    participant: ["[class*=\"newGridItem\"]"],
    participant_name: ["[class*=\"participantName\"]", "[class*=\"TextName\"]"],
    speaking_marker: ["[class*=\"rootStroke\"]"],
    participant_key_attr: ["attr:data-participant-id"],
    active_speaker: []
};

// --- перебор кандидатов -----------------------------------------------------

test("кандидаты перебираются по порядку, побеждает первый попавший", () => {
    const doc = el("body", {}, [el("button", { id: "second" }, [], "Войти")]);
    const hit = forgeProbeHit(doc, ["#first", "#second", "#third"]);
    assert.equal(hit.index, 1, "сработал не тот кандидат");
    assert.equal(hit.candidate, "#second");
    assert.equal(hit.el.attrs.id, "second");
});

test("первый попавший останавливает перебор, даже если дальше есть точнее", () => {
    const doc = el("body", {}, [
        el("button", { class: "a" }, [], "первая"),
        el("button", { class: "b" }, [], "вторая")
    ]);
    assert.equal(forgeProbeFirst(doc, [".a", ".b"]).attrs.class, "a");
    assert.equal(forgeProbeFirst(doc, [".b", ".a"]).attrs.class, "b");
});

test("промах всех кандидатов и пустой массив дают null, а не исключение", () => {
    const doc = el("body", {}, []);
    assert.equal(forgeProbeHit(doc, ["#нет", ".тоже-нет"]), null);
    assert.equal(forgeProbeHit(doc, []), null);
    assert.equal(forgeProbeFirst(doc, undefined), null);
    assert.equal(forgeProbePresent(doc, []), false);
});

test("БИТЫЙ селектор из пользовательского TOML не роняет пробу, а уступает следующему кандидату", () => {
    const doc = el("body", {}, [el("button", { id: "join" }, [], "Войти")]);
    // Именно так выглядит забытая кавычка в `[class*=`.
    assert.throws(() => doc.querySelectorAll("[class*=\"newGridItem"), SyntaxError);
    assert.deepEqual(forgeProbeAll(doc, "[class*=\"newGridItem"), []);
    const hit = forgeProbeHit(doc, ["[class*=\"newGridItem", "#join"]);
    assert.equal(hit.index, 1, "битый кандидат обязан пропускаться, а не обрывать перебор");
});

test("поиск по тексту берёт только кликабельные элементы и не различает регистр", () => {
    const doc = el("body", {}, [
        el("div", {}, [], "Продолжить в браузере"),
        el("button", { id: "real" }, [], "  ПРОДОЛЖИТЬ В браузере  ")
    ]);
    const hit = forgeProbeHit(doc, ["text:продолжить в браузере"]);
    assert.equal(hit.el.attrs.id, "real", "текст нашёлся на некликабельном предке");
    assert.equal(forgeProbeHit(doc, ["text:такого текста нет"]), null);
    assert.equal(forgeText(hit.el), "ПРОДОЛЖИТЬ В браузере", "пробелы обязаны схлопываться");
});

test("значение поля читается из value, а не из текста", () => {
    const input = el("input", { type: "submit", value: "Присоединиться" });
    assert.equal(forgeText(input), "Присоединиться");
    assert.equal(forgeProbeHit(el("body", {}, [input]), ["text:присоединиться"]).el, input);
});

// --- видимость --------------------------------------------------------------

test("НЕВИДИМЫЙ элемент пробой не считается: пре-join и панель встречи живут в одном документе", () => {
    const hidden = el("button", { id: "leave" }, [], "Выйти");
    hidden.getBoundingClientRect = () => ({ width: 0, height: 0 });
    const shown = el("button", { id: "join" }, [], "Присоединиться");
    shown.getBoundingClientRect = () => ({ width: 120, height: 32 });
    const doc = el("body", {}, [hidden, shown]);

    assert.equal(forgeVisible(hidden), false);
    assert.equal(forgeVisible(shown), true);
    assert.deepEqual(forgeProbeAll(doc, "#leave"), [], "спрятанная кнопка выхода не имеет права находиться");
    assert.equal(forgeProbeFirst(doc, ["#leave", "#join"]).attrs.id, "join");
    // И по тексту тоже: клик по невидимой кнопке всё равно промахнётся.
    assert.equal(forgeProbeHit(doc, ["text:выйти"]), null);
});

test("ВЫКЛЮЧЕННАЯ кнопка пробой не считается — иначе бот жмёт её по кругу", () => {
    // Ровно этот случай сломал первый живой прогон входа: мок-встреча после
    // клика оставляет «Продолжить в браузере» в разметке с `disabled`, и бот
    // двадцать секунд жал её вместо того, чтобы ввести имя.
    const pressed = el("button", { id: "continue" }, [], "Продолжить в браузере");
    pressed.disabled = true;
    const aria = el("button", { id: "aria", "aria-disabled": "true" }, [], "Присоединиться");
    const live = el("button", { id: "join" }, [], "Присоединиться");
    const doc = el("body", {}, [pressed, aria, live]);

    assert.equal(forgeVisible(pressed), false, "disabled кнопка не кликается");
    assert.equal(forgeVisible(aria), false, "aria-disabled тоже");
    assert.deepEqual(forgeProbeAll(doc, "#continue"), []);
    assert.equal(forgeProbeHit(doc, ["text:присоединиться"]).el.attrs.id, "join");
});

test("среда без layout считается видимой: тесты проверяют пробы, а не отрисовку", () => {
    const doc = el("body", {}, [el("button", { id: "j" }, [], "Войти")]);
    assert.equal(forgeVisible(doc.querySelector("#j")), true);
});

// --- участники --------------------------------------------------------------

test("участники мок-встречи: имя из вложенной пробы, ключ из атрибута, речь с САМОЙ плитки", () => {
    const doc = el("body", {}, [mockTile("p0", "F.O.R.G.E.", false), mockTile("p1", "Алиса", true)]);
    const got = forgeReadParticipants(doc, MOCK_PROBES);
    assert.deepEqual(got, [
        { key: "p0", name: "F.O.R.G.E.", speaking: false },
        { key: "p1", name: "Алиса", speaking: true }
    ]);
});

test("участники Телемоста: маркер речи в ПОТОМКЕ плитки, классы подстрочно", () => {
    const doc = el("body", {}, [telemostTile("Максим К.", true), telemostTile("Игорь Г.", false)]);
    const got = forgeReadParticipants(doc, TELEMOST_PROBES);
    assert.equal(got.length, 2);
    assert.equal(got[0].name, "Максим К.");
    assert.equal(got[0].speaking, true, "обводка rootStroke на потомке не увидена");
    assert.equal(got[1].speaking, false);
    assert.equal(got[0].key, null, "у Телемоста стабильного идентификатора плитки нет");
});

test("СВОЯ ПЛИТКА ВЫЧИТАЕТСЯ: бот не участник самому себе", () => {
    const doc = el("body", {}, [
        livekitTile("F.O.R.G.E. бот", { local: true }),
        livekitTile("Алиса Тестова", { speaking: true }),
        livekitTile("Борис Пробин")
    ]);
    const got = forgeReadParticipants(doc, VCC_PROBES);
    assert.deepEqual(got.map(p => p.name), ["Алиса Тестова", "Борис Пробин"],
        "плитка бота попала в список участников");
    assert.equal(got[0].speaking, true);
    assert.equal(got[0].key, null, "SID участника в DOM не выставлен — ключ берётся именем в Rust");
});

test("ДЕМОНСТРАЦИЯ ЭКРАНА НЕ УЧАСТНИК: плитка screen_share не попадает в список", () => {
    // Живая встреча пользователя: в списке участников оказались «Игорь Г's
    // screen» и «Игорь's screen», а в расшифровке 15 реплик из 78 были
    // подписаны экраном. Плитка трансляции стоит в DOM РАНЬШЕ плитки камеры
    // хозяина — порядок здесь такой же, как живьём.
    const doc = el("body", {}, [
        livekitTile("F.O.R.G.E. бот", { local: true }),
        livekitScreenTile("Игорь", { speaking: true }),
        livekitTile("Игорь", { speaking: true }),
        livekitTile("Пётр Демонстратор")
    ]);
    const got = forgeReadParticipants(doc, VCC_PROBES);
    assert.deepEqual(got.map(p => p.name), ["Игорь", "Пётр Демонстратор"],
        "трансляция экрана посчиталась человеком");
    assert.equal(got.filter(p => p.name.includes("'s screen")).length, 0);
});

test("активным спикером становится человек, а не его экран", () => {
    // У площадки проба активного спикера сужена тем же атрибутом, но
    // крошечный DOM не умеет `:not(...)`, поэтому здесь проверяется ЗАПАСНОЙ
    // путь: элемента активного спикера нет, и имя берётся у говорящей плитки
    // из списка участников. Список экрана уже не содержит — значит и активным
    // спикером он стать не может.
    const doc = el("body", {}, [
        livekitScreenTile("Игорь", { speaking: true }),
        livekitTile("Игорь", { speaking: true })
    ]);
    const parts = forgeReadParticipants(doc, VCC_PROBES);
    assert.equal(forgeReadActiveSpeaker(doc, VCC_PROBES, parts), "Игорь");
});

test("пустая проба своей плитки НИЧЕГО не вычитает: у прочих площадок список прежний", () => {
    const doc = el("body", {}, [mockTile("p0", "F.O.R.G.E.", false), mockTile("p1", "Алиса", true)]);
    assert.equal(forgeReadParticipants(doc, MOCK_PROBES).length, 2);
    assert.equal(forgeReadParticipants(doc, { ...MOCK_PROBES, local_participant_marker: [] }).length, 2);
});

test("протухшая проба своей плитки не обнуляет список участников", () => {
    // Худший случай механизма: LiveKit переименовал атрибут. Вычитать станет
    // нечего — и это ровно та деградация, которая допустима: бот снова
    // попадёт в собственный список, но остальные участники не пропадут.
    const doc = el("body", {}, [
        livekitTile("F.O.R.G.E. бот", { local: true }),
        livekitTile("Алиса Тестова")
    ]);
    const rotten = { ...VCC_PROBES, local_participant_marker: ["[data-lk-нет-такого=\"true\"]"] };
    assert.deepEqual(forgeReadParticipants(doc, rotten).map(p => p.name),
        ["F.O.R.G.E. бот", "Алиса Тестова"]);
});

test("протухшая проба имени не обнуляет список: имя берётся текстом плитки", () => {
    const doc = el("body", {}, [mockTile("p1", "Алиса", false)]);
    const probes = { ...MOCK_PROBES, participant_name: ["[data-testid=\"нет-такого\"]"] };
    const got = forgeReadParticipants(doc, probes);
    assert.equal(got.length, 1);
    assert.equal(got[0].name, "Алиса", "текст плитки обязан отработать запасным путём");
});

test("маркер речи и ключ участника устойчивы к мусорным кандидатам", () => {
    const tile = mockTile("p9", "Борис", true);
    assert.equal(forgeMarked(tile, ["[битый", "[data-speaking=\"true\"]"]), true);
    assert.equal(forgeMarked(tile, ["text:Борис"]), false, "текстовый кандидат не маркер");
    assert.equal(forgeKeyAttr(tile, ["attr:data-uid", "attr:data-participant-id"]), "p9");
    assert.equal(forgeKeyAttr(tile, ["attr:data-uid"]), null);
});

// --- активный спикер --------------------------------------------------------

test("прочерк в элементе активного спикера означает «никто не говорит»", () => {
    const doc = el("body", {}, [el("p", { "data-testid": "active-speaker" }, [], "—")]);
    assert.equal(forgeReadActiveSpeaker(doc, MOCK_PROBES, []), null);

    const talking = el("body", {}, [el("p", { "data-testid": "active-speaker" }, [], "Алиса")]);
    assert.equal(forgeReadActiveSpeaker(talking, MOCK_PROBES, []), "Алиса");
});

test("без элемента активного спикера имя берётся у говорящей плитки — путь Телемоста", () => {
    const doc = el("body", {}, [telemostTile("Максим К.", false), telemostTile("Игорь Г.", true)]);
    const participants = forgeReadParticipants(doc, TELEMOST_PROBES);
    assert.equal(forgeReadActiveSpeaker(doc, TELEMOST_PROBES, participants), "Игорь Г.");
});

// --- снимок целиком ---------------------------------------------------------

test("снимок мок-встречи: участники, микрофон, камера, конец встречи", () => {
    const doc = el("body", {}, [
        mockTile("p0", "F.O.R.G.E.", false),
        mockTile("p1", "Алиса", true),
        el("p", { "data-testid": "active-speaker" }, [], "Алиса"),
        el("button", { "data-testid": "mic-toggle", "data-enabled": "true" }, [], "Микрофон включён"),
        el("button", { "data-testid": "cam-toggle", "data-enabled": "false" }, [], "Камера выключена"),
        el("div", { "data-testid": "meeting-ended", "data-ended": "false" }, [], "Встреча завершена")
    ]);
    const snap = forgeObserveMeeting(doc, MOCK_PROBES, { href: "http://127.0.0.1:1/mock" }, { perf: 1234.5, unix: 17 });
    assert.equal(snap.participants.length, 2);
    assert.equal(snap.active, "Алиса");
    assert.equal(snap.speaking, true);
    assert.equal(snap.mic, "on", "кнопка «выключить» видна — значит микрофон включён");
    assert.equal(snap.cam, "off");
    assert.equal(snap.ended, false, "маркер есть в разметке, но выключен атрибутом");
    assert.equal(snap.postCall, false);
    assert.equal(snap.perf, 1234.5);
    assert.equal(snap.unix, 17);
});

test("маркер конца встречи срабатывает ровно тогда, когда встреча кончилась", () => {
    const doc = el("body", {}, [el("div", { "data-testid": "meeting-ended", "data-ended": "true" }, [], "Встреча завершена")]);
    const snap = forgeObserveMeeting(doc, MOCK_PROBES, { href: "http://х/mock" }, {});
    assert.equal(snap.ended, true);
});

test("пост-звонковый адрес — подстрока, а не селектор", () => {
    assert.equal(forgePostCallUrl("https://х/after-call?id=1", ["/after-call"]), true);
    assert.equal(forgePostCallUrl("https://х/mock", ["/after-call"]), false);
    assert.equal(forgePostCallUrl("https://х/mock", []), false, "пустая проба обязана молчать");
});

test("ПОЛНАЯ ДЕГРАДАЦИЯ: пустой набор проб даёт пустой снимок, а не исключение", () => {
    const doc = el("body", {}, [mockTile("p1", "Алиса", true)]);
    const snap = forgeObserveMeeting(doc, {}, { href: "https://telemost.yandex.ru/j/1" }, { perf: 5, unix: 6 });
    assert.deepEqual(snap.participants, [], "без проб участников знать неоткуда");
    assert.equal(snap.active, null);
    assert.equal(snap.mic, "unknown", "«не знаю» — не то же самое, что «выключен»");
    assert.equal(snap.cam, "unknown");
    assert.equal(snap.ended, false, "нет пробы конца — нет и конца встречи");
    assert.equal(snap.ui.joinButton, false);
    assert.equal(snap.href, "https://telemost.yandex.ru/j/1");
});

// --- `contains:` — маркеры, которых `text:` не находил НИКОГДА ---------------

test("contains: находит маркер в обычном div — то, чего text: не умел вовсе", () => {
    const doc = el("body", {}, [
        el("div", { id: "banner" }, [
            el("span", {}, [], "Организатор отклонил ваш запрос на присоединение")
        ])
    ]);
    // `text:` перебирает ТОЛЬКО кликабельное, а маркер — обычный span.
    assert.equal(forgeProbePresent(doc, ["text:Организатор отклонил"]), false,
        "если это стало правдой, ловушки больше нет и тест пора переписать");
    assert.equal(forgeProbePresent(doc, ["contains:Организатор отклонил"]), true);
});

test("contains: берёт САМЫЙ ГЛУБОКИЙ носитель текста, а не предка во весь экран", () => {
    const leaf = el("span", {}, [], "Вы вышли из собрания");
    const doc = el("body", {}, [el("div", { id: "wrap" }, [el("div", { id: "inner" }, [leaf])])]);
    const hits = forgeProbeAll(doc, "contains:Вы вышли");
    assert.equal(hits.length, 1, "предки не должны попадать в выборку");
    assert.equal(hits[0], leaf);
});

test("contains: не различает регистр и терпит переносы строк", () => {
    const doc = el("body", {}, [el("p", {}, [], "  Someone   in the meeting\nshould let you in soon ")]);
    assert.equal(forgeProbePresent(doc, ["contains:SOMEONE IN THE MEETING SHOULD LET YOU IN"]), true);
});

test("contains: пропускает длинные тексты — предок с половиной страницы маркером не бывает", () => {
    const long = "x".repeat(500) + " Вы вышли из собрания";
    const doc = el("body", {}, [el("div", {}, [], long)]);
    assert.equal(forgeProbePresent(doc, ["contains:Вы вышли"]), false);
});

test("contains: НЕ считается маркером плитки — иначе имя участника делало бы его говорящим", () => {
    const tile = el("div", {}, [], "Алиса");
    assert.equal(forgeMarked(tile, ["contains:Алиса"]), false,
        "contains: — маркер документа, а не признак элемента");
});

// --- отказ во входе и кнопка звука ------------------------------------------

test("ОТКАЗ ВО ВХОДЕ — отдельное поле снимка, а не «ничего не нашли»", () => {
    const probes = { join_denied_marker: ["contains:Вам отказано в доступе"] };
    const denied = el("body", {}, [el("div", {}, [], "Вам отказано в доступе к собранию")]);
    assert.equal(forgeObserveMeeting(denied, probes, {}, {}).denied, true);

    const normal = el("body", {}, [el("div", {}, [], "Идёт собрание")]);
    assert.equal(forgeObserveMeeting(normal, probes, {}, {}).denied, false);

    // Без пробы отказа не бывает: пустая проба обязана молчать, а не «не знаю».
    assert.equal(forgeObserveMeeting(denied, {}, {}, {}).denied, false);
});

test("кнопка «подключить звук» видна снимку — без неё у Zoom записывать нечего", () => {
    const probes = { join_audio_button: ["[data-testid=\"join-audio\"]"] };
    const withBtn = el("body", {}, [
        el("button", { "data-testid": "join-audio" }, [], "Войти с использованием звука компьютера")
    ]);
    assert.equal(forgeObserveMeeting(withBtn, probes, {}, {}).ui.joinAudio, true);
    assert.equal(forgeObserveMeeting(el("body", {}, []), probes, {}, {}).ui.joinAudio, false);
    // У площадок без такой кнопки проба пуста — шага просто не существует.
    assert.equal(forgeObserveMeeting(withBtn, {}, {}, {}).ui.joinAudio, false);
});
