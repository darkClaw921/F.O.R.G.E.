# plugins/echo/src/actions/parser.rs

Парсер forge-actions blocks из ответа Claude. extract(text) ищет fenced code blocks с языком 'forge-actions', парсит body как JSON-array или single Action, возвращает Vec<Action>. iter_fenced_blocks реализует простую state-machine без зависимостей. Толерантен: невалидный JSON → warn + skip; неизвестный SystemActionKind → warn + skip (через serde Err); несколько блоков конкатенирует; whitespace вокруг body триммируется.
