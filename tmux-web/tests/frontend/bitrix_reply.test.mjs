// Регресс-тест ЧИСТОЙ ЛОГИКИ ответов панели «Битрикс24» (forge-92x6).
//
// Грузит РЕАЛЬНЫЙ модуль `js/bitrix/reply.js` без всякого DOM: и разбор
// цитаты, и поиск оригинала — чистые функции над сегментами и списком
// сообщений, и стенд с фейковым документом им не нужен.
//
// Что покрыто:
//   • цитата вида `> Автор: текст\nответ` (её приписывает сервер,
//     `routes/dialogs.rs::compose_reply`) отделяется от тела ответа;
//   • цитата `[QUOTE]…[/QUOTE]` (веб-клиент портала) — тоже;
//   • сообщение, состоящее ИЗ ОДНОЙ цитаты, цитатой НЕ считается: иначе его
//     текст исчез бы в узкой серой строке;
//   • оригинал находится по автору и НАЧАЛУ текста — обрезка сервером до 200
//     символов с «…» совпадению не мешает;
//   • оригинал ищется строго СТАРШЕ ответа: иначе цитата нашла бы сама себя;
//   • неотправленные строки (pending/failed) оригиналом не бывают;
//   • двоеточие в тексте, принятое за автора, не ломает переход — работает
//     запасное совпадение по целой строке цитаты.
//
// Запуск: node tmux-web/tests/frontend/bitrix_reply.test.mjs
// Код возврата 0 — все ассерты прошли, 1 — есть [FAIL].

const reply = await import(new URL('../../static/js/bitrix/reply.js', import.meta.url).href);

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

/** Плоский текстовый сегмент. */
function seg(text) {
    return { kind: 'text', text, value: null, children: [] };
}

/** Сообщение ленты — ровно те поля, которых касается reply.js. */
function msg(id, name, text, extra = {}) {
    return {
        id,
        sender: { id: String(id), name },
        system: false,
        text,
        segments: [seg(text)],
        ...extra,
    };
}

// --- splitQuote: форма compose_reply ---------------------------------------

const own = reply.splitQuote([seg('> Игорь Герасимов: где отчёт по спринту\nсдал вчера')]);
ok(own !== null, 'цитата `> Автор: текст` распознана');
eq(own && own.quote.author, 'Игорь Герасимов', 'автор цитаты отделён');
eq(own && own.quote.text, 'где отчёт по спринту', 'текст цитаты отделён');
eq(own && own.body.map((s) => s.text), ['сдал вчера'], 'тело ответа осталось без цитаты');

const noAuthor = reply.splitQuote([seg('> просто текст\nответ')]);
eq(noAuthor && noAuthor.quote.author, '', 'цитата без подписи автора не выдумывает его');
eq(noAuthor && noAuthor.quote.text, 'просто текст', 'вся цитата ушла в текст');

const multi = reply.splitQuote([seg('> Игорь: первая\n> вторая строка\nответ')]);
eq(multi && multi.quote.text, 'первая вторая строка', 'многострочная цитата схлопнута в одну');
eq(multi && multi.body.map((s) => s.text), ['ответ'], 'тело многострочной цитаты');

eq(reply.splitQuote([seg('обычное сообщение')]), null, 'сообщение без цитаты — не ответ');
eq(reply.splitQuote([seg('> одна лишь цитата')]), null, 'сообщение ИЗ ОДНОЙ цитаты — не ответ');
eq(reply.splitQuote([]), null, 'пустые сегменты — не ответ');
eq(reply.splitQuote(null), null, 'отсутствие сегментов — не ответ');

// Хвост сегментов (жирное, ссылка) остаётся в теле, а не теряется.
const tail = reply.splitQuote([
    seg('> Игорь: смотри\n'),
    { kind: 'b', text: '', value: null, children: [seg('важное')] },
]);
ok(tail !== null, 'цитата с форматированным хвостом распознана');
eq(tail && tail.body.length, 1, 'хвостовой сегмент остался в теле');
eq(tail && tail.body[0].kind, 'b', 'форматирование тела не потеряно');

// --- splitQuote: форма [QUOTE] портала --------------------------------------

const portal = reply.splitQuote([
    { kind: 'quote', text: '', value: null, children: [seg('Игорь: где отчёт')] },
    { kind: 'br', text: '', value: null, children: [] },
    seg('сдал вчера'),
]);
ok(portal !== null, 'цитата [QUOTE] распознана');
eq(portal && portal.quote.author, 'Игорь', 'автор из [QUOTE]');
eq(portal && portal.body.map((s) => s.text), ['сдал вчера'], 'перевод строки за [QUOTE] отброшен');

eq(
    reply.splitQuote([{ kind: 'quote', text: '', value: null, children: [seg('одна цитата')] }]),
    null,
    '[QUOTE] без ответа — не ответ',
);

// --- findQuoteOrigin --------------------------------------------------------

const list = [
    msg(10, 'Игорь Герасимов', 'где отчёт по спринту'),
    msg(12, 'Анна', 'ок'),
    msg(14, 'Анна', 'сдал вчера'),
];
const quote = reply.parseQuoteBody('Игорь Герасимов: где отчёт по спринту');
const found = reply.findQuoteOrigin(list, 14, quote);
eq(found && found.id, 10, 'оригинал найден по автору и тексту');

// Обрезка сервером: превью кончается многоточием, оригинал — нет.
const long = 'а'.repeat(reply.QUOTE_MAX_CHARS + 40);
const longList = [msg(20, 'Игорь', long), msg(22, 'Анна', 'ответ')];
const cut = reply.parseQuoteBody(`Игорь: ${long.slice(0, reply.QUOTE_MAX_CHARS - 1)}…`);
const foundLong = reply.findQuoteOrigin(longList, 22, cut);
eq(foundLong && foundLong.id, 20, 'обрезанная цитата совпала с началом оригинала');

// Строго СТАРШЕ ответа: сама себя цитата найти не должна.
const selfList = [msg(30, 'Игорь', '> Игорь: где отчёт\nсдал')];
eq(reply.findQuoteOrigin(selfList, 30, quote), null, 'цитата не находит собственную строку');

// Ближайший из двух одинаковых — тот, что ниже.
const dupes = [msg(40, 'Анна', 'ок'), msg(42, 'Анна', 'ок'), msg(44, 'Игорь', 'ответ')];
const dupe = reply.findQuoteOrigin(dupes, 44, reply.parseQuoteBody('Анна: ок'));
eq(dupe && dupe.id, 42, 'из одинаковых реплик выбирается ближайшая к ответу');

// Неотправленная строка оригиналом не бывает: у неё временный идентификатор.
const pending = [
    msg(50, 'Анна', 'ок', { pending: true }),
    msg(48, 'Анна', 'ок'),
    msg(52, 'Игорь', 'ответ'),
];
const skipped = reply.findQuoteOrigin(pending, 52, reply.parseQuoteBody('Анна: ок'));
eq(skipped && skipped.id, 48, 'строка «отправляется» пропущена');

// Двоеточие в тексте принято за автора — спасает запасное совпадение по целой
// строке цитаты.
const colon = [msg(60, 'Анна', 'Внимание: смотри вложение'), msg(62, 'Игорь', 'ответ')];
const colonQuote = reply.parseQuoteBody('Внимание: смотри вложение');
eq(colonQuote.author, 'Внимание', 'двоеточие посреди текста разобрано как автор (ожидаемо)');
const colonFound = reply.findQuoteOrigin(colon, 62, colonQuote);
eq(colonFound && colonFound.id, 60, 'запасное совпадение по целой цитате сработало');

// Автор не совпал и текста такого нет — оригинала нет.
eq(
    reply.findQuoteOrigin(list, 14, reply.parseQuoteBody('Пётр: чего-то не было')),
    null,
    'несуществующий оригинал не выдумывается',
);

// --- нормализация -----------------------------------------------------------

eq(reply.normalizeQuoteText('  два\nряда   пробелов '), 'два ряда пробелов', 'пробелы схлопнуты');
eq(reply.stripEllipsis('обрезано…'), 'обрезано', 'многоточие обрезки снято');
eq(reply.stripEllipsis('целое'), 'целое', 'необрезанный текст не тронут');

console.log(`bitrix_reply: проверок ${checks}, провалов ${failures}`);
process.exit(failures === 0 ? 0 : 1);
