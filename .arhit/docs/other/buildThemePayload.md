# buildThemePayload

Phase 5: формирование payload для POST/PUT custom theme.

Сигнатура: buildThemePayload(draft: {id,name,ui,term}, isEdit: boolean) → ThemeDTO

Возвращает: { id, name, kind: 'custom', ui: {...}, term: {...} } — camelCase shape, который ожидает бэкенд (Theme/UiColors/TermColors имеют #[serde(rename_all = camelCase)]).

- В create-режиме (isEdit=false): id оставлен пустым ('') — сервер сгенерирует UUID v4 (см. create_custom_theme в main.rs).
- В edit-режиме (isEdit=true): id из draft.id, но сервер всё равно его игнорирует и берёт из path-параметра PUT /api/themes/custom/:id.
- name проходит trim().
- ui/term — копируются через spread (защита от мутации после save).

Используется в Save handler внутри openThemeEditor.
