# pick_primary_gen

Выбирает primary-индекс среди members для дедупликации is_generating (tmux-web/src/attention.rs, private fn).

Структурно-парная функция к pick_primary, но критерий 'кандидата' — items[i].changed == true вместо detected. Даёт независимую от permission-prompt'а ось выбора: deduplicate_generating решает, в какой именно сессии группы зажечь индикатор 'генерирует'.

## Сигнатура

fn pick_primary_gen(items: &[GenSnapshot], members: &[usize]) -> Option<usize>

## Приоритет (первое сработавшее правило выбирает primary)

1. среди элементов с changed=true и attached>0 — наибольший id лексикографически (приоритет тому, что кто-то реально смотрит);
2. иначе среди всех changed=true — наибольший id (свежесозданная сессия предпочтительнее);
3. fallback: лексикографически наибольшее имя среди changed=true (на практике недостижимо, т.к. tmux session id уникальны).

## Возврат

Some(idx) — индекс primary в items.
None — ни у одной сессии в группе changed != true. Дедуп не должен зажигать индикатор 'из ничего'.

## Почему не обобщение pick_primary

Намеренно не пытается обобщить pick_primary через предикат-замыкание — просто копирует и адаптирует. Причины: (1) не ломать существующие тесты pick_primary; (2) держать обе функции независимыми друг от друга при будущих изменениях семантики (например, добавление веса по recent activity для is_generating, но не для needs_attention).

## Зависимости

Вызывается из deduplicate_generating. Не имеет внешних зависимостей кроме GenSnapshot.
