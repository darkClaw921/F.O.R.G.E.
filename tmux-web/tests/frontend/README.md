# Frontend regression tests

Standalone Node-based regression tests for `tmux-web/static/app.js`.
No package manager / no jest — pure Node, just run the file directly.

## Running

```bash
node tmux-web/tests/frontend/sidebar_grouping.test.js
```

Exit code 0 — all assertions pass.
Exit code 1 — at least one assertion failed (see `[FAIL ...]` lines).

## Files

| File | What it covers |
| --- | --- |
| `sidebar_grouping.test.js` | Phase 6 / forge-cca8.2 — группировка sessions внутри origin'а, двухуровневая фильтрация origin → project, contract `aggregateAllOrigins`, projectFilter не сбрасывается при смене origin. |

## Контракт

Тесты реплицируют **pure-логику** из `static/app.js` (без DOM). Это значит,
что при изменении любой из функций ниже — нужно одновременно поправить и
ассерты в тестовых файлах:

- `groupSessionsByProject(sessions, orphanKey)` — экспортирована в
  `window.__forge.groupSessionsByProject`;
- `aggregateAllOrigins()` — экспортирована в `window.__forge.aggregateAllOrigins`;
- логика "какие origin'ы видны" в `renderSidebarWithOrigin`;
- логика двухуровневой фильтрации в `renderOriginSection`.

Если упал тест после правки `app.js` — это сигнал что меняется публичный
контракт, проверь что:
1. backend/UI остаются совместимы;
2. документация в `arhit doc` обновлена;
3. legacy режим (`remote_mode=false`, `renderSidebar` без `renderSidebarWithOrigin`)
   не задет — это явное требование Phase 6.
