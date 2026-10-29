// Регрессионный тест чистой логики редактора тегов карточки трекера
// (`static/js/tracker/tags.js`).
//
// ГЛАВНОЕ, ЧТО ЗДЕСЬ ЛОВИТСЯ:
//
//   1. РЕШЁТКА НЕ ЗАДВАИВАЕТСЯ. Человек набирает `#forge`, чип рисует свою
//      решётку, в модели тег хранится без неё. Перепутать уровни — значит
//      получить `##forge` в задаче и вторую строку в сайдбаре;
//   2. ДУБЛЬ В ДРУГОМ РЕГИСТРЕ — ЭТО ДУБЛЬ. `Forge` и `forge` обязаны быть
//      одним тегом (иначе фильтр в сайдбаре показывает половину задач), но
//      уже сохранённое написание не переписывается;
//   3. ПОРЯДОК СОХРАНЯЕТСЯ. `buildPatch` сравнивает списки строкой, и
//      перестановка тегов означала бы лишний PATCH на пустом месте;
//   4. ПУНКТ «СОЗДАТЬ НОВЫЙ» ПОЯВЛЯЕТСЯ ТОЛЬКО ДЛЯ НЕИЗВЕСТНОГО ТЕГА и
//      никогда — для уже навешенного (это предложение завести дубль);
//   5. ПОДСКАЗКИ БЕРУТСЯ ИЗ ЗАДАЧ СНИМКА — того же источника, что питает
//      блок `// TAGS` в сайдбаре. Второго источника у тегов нет.
//
// Запуск: node --test tmux-web/tests/frontend/tracker_tags.test.mjs

import test from 'node:test';
import assert from 'node:assert/strict';

import {
    SUGGEST_LIMIT, addTag, hasTag, knownTags, moveSuggest, normalizeTag,
    popTag, removeTagAt, splitTags, suggestTags,
} from '../../static/js/tracker/tags.js';

// ------------------------------------------------------------- нормализация

test('нормализация: ведущая решётка срезается и не задваивается', () => {
    assert.equal(normalizeTag('forge'), 'forge');
    assert.equal(normalizeTag('#forge'), 'forge');
    assert.equal(normalizeTag('##forge'), 'forge');
    assert.equal(normalizeTag('  #forge  '), 'forge');
});

test('нормализация: пустое и одна решётка — это «добавлять нечего»', () => {
    assert.equal(normalizeTag(''), '');
    assert.equal(normalizeTag('   '), '');
    assert.equal(normalizeTag('#'), '');
    assert.equal(normalizeTag('###'), '');
    assert.equal(normalizeTag(null), '');
    assert.equal(normalizeTag(undefined), '');
});

test('нормализация: регистр НЕ приводится — чужое написание не переписываем', () => {
    assert.equal(normalizeTag('#Forge'), 'Forge');
    assert.equal(normalizeTag('ДИЗАЙН'), 'ДИЗАЙН');
});

test('разбор строки: пробелы и запятые — разделители', () => {
    assert.deepEqual(splitTags('#a, b  #c'), ['a', 'b', 'c']);
    assert.deepEqual(splitTags(''), []);
    assert.deepEqual(splitTags('  ,  '), []);
    assert.deepEqual(splitTags(null), []);
});

// ----------------------------------------------------------------- добавление

test('добавление: тег уходит в конец, исходный массив не меняется', () => {
    const before = ['forge', 'ui'];
    const { tags, added, tag } = addTag(before, '#дизайн');
    assert.deepEqual(tags, ['forge', 'ui', 'дизайн']);
    assert.equal(added, true);
    assert.equal(tag, 'дизайн');
    assert.deepEqual(before, ['forge', 'ui'], 'исходный список не мутирован');
});

test('добавление: дубль в другом регистре не добавляется', () => {
    const r = addTag(['Forge'], '#forge');
    assert.deepEqual(r.tags, ['Forge'], 'сохранённое написание не переписано');
    assert.equal(r.added, false);
});

test('добавление: пустой тег не добавляется', () => {
    assert.equal(addTag(['a'], '#').added, false);
    assert.equal(addTag(['a'], '   ').added, false);
    assert.deepEqual(addTag(['a'], '').tags, ['a']);
});

test('добавление: порядок существующих тегов не меняется', () => {
    const r = addTag(['я', 'а', 'б'], 'в');
    assert.deepEqual(r.tags, ['я', 'а', 'б', 'в']);
});

// -------------------------------------------------------------------- снятие

test('снятие по индексу: крестик убирает ровно свой чип', () => {
    assert.deepEqual(removeTagAt(['a', 'b', 'c'], 1), ['a', 'c']);
});

test('снятие по индексу: чужой индекс ничего не ломает', () => {
    assert.deepEqual(removeTagAt(['a'], 5), ['a']);
    assert.deepEqual(removeTagAt(['a'], -1), ['a']);
    assert.deepEqual(removeTagAt([], 0), []);
});

test('Backspace на пустом вводе снимает последний чип', () => {
    assert.deepEqual(popTag(['a', 'b']), ['a']);
    assert.deepEqual(popTag(['a']), []);
    assert.deepEqual(popTag([]), []);
});

test('hasTag сравнивает без учёта регистра и решётки', () => {
    assert.equal(hasTag(['Forge'], '#forge'), true);
    assert.equal(hasTag(['Forge'], 'ui'), false);
    assert.equal(hasTag([], 'a'), false);
    assert.equal(hasTag(['a'], '#'), false);
});

// ------------------------------------------------------- известные теги (снимок)

test('известные теги считаются из задач снимка, по алфавиту и без дублей', () => {
    const tasks = [
        { tags: ['ui', 'forge'] },
        { tags: ['Forge'] },
        { tags: [] },
        {},
        null,
    ];
    assert.deepEqual(knownTags(tasks), ['forge', 'ui']);
});

test('известные теги: мусор в снимке не роняет расчёт', () => {
    assert.deepEqual(knownTags(null), []);
    assert.deepEqual(knownTags([{ tags: ['#a', '  ', '#'] }]), ['a']);
});

// ------------------------------------------------------------------ подсказки

const KNOWN = ['backlog', 'design', 'forge', 'log', 'refactor', 'tracker', 'ui'];

test('подсказки: пустой ввод показывает все известные теги', () => {
    const s = suggestTags(KNOWN, '', []);
    assert.deepEqual(s.items, KNOWN);
    assert.equal(s.create, null, 'пустой ввод ничего не «создаёт»');
});

test('подсказки: совпадение с начала стоит выше совпадения внутри слова', () => {
    const s = suggestTags(KNOWN, 'log', []);
    assert.deepEqual(s.items, ['log', 'backlog']);
});

test('подсказки: уже навешенные теги из списка вычеркнуты', () => {
    const s = suggestTags(KNOWN, '', ['Forge']);
    assert.deepEqual(s.items, ['backlog', 'design', 'log', 'refactor', 'tracker', 'ui']);
});

test('подсказки: неизвестный тег даёт пункт «создать новый»', () => {
    const s = suggestTags(KNOWN, '#релиз', ['forge']);
    assert.deepEqual(s.items, []);
    assert.equal(s.create, 'релиз');
    assert.equal(s.query, 'релиз');
});

test('подсказки: известный тег «создавать» не предлагается даже в другом регистре', () => {
    assert.equal(suggestTags(KNOWN, 'FORGE', []).create, null);
    assert.equal(suggestTags(KNOWN, 'forge', []).create, null);
});

test('подсказки: уже навешенный тег «создавать» не предлагается', () => {
    const s = suggestTags([], 'релиз', ['Релиз']);
    assert.deepEqual(s.items, []);
    assert.equal(s.create, null, 'это предложение завести дубль');
});

test('подсказки: список ограничен по длине', () => {
    const many = Array.from({ length: 30 }, (_, i) => `t${String(i).padStart(2, '0')}`);
    assert.equal(suggestTags(many, 't', []).items.length, SUGGEST_LIMIT);
    assert.equal(suggestTags(many, 't', [], 3).items.length, 3);
});

// ------------------------------------------------------------------ стрелки

test('стрелки ходят по подсказкам по кольцу', () => {
    assert.equal(moveSuggest(-1, 1, 3), 0);
    assert.equal(moveSuggest(0, 1, 3), 1);
    assert.equal(moveSuggest(2, 1, 3), 0);
    assert.equal(moveSuggest(0, -1, 3), 2);
    assert.equal(moveSuggest(-1, -1, 3), 2);
});

test('стрелки по пустому списку никуда не ведут', () => {
    assert.equal(moveSuggest(0, 1, 0), -1);
    assert.equal(moveSuggest(-1, -1, 0), -1);
});
