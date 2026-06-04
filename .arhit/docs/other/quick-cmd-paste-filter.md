# quick-cmd-paste-filter

Защита freq-трекинга команд от мусора (Command dock / quick-cmd).

## Проблема
В «частые команды» попадал хлам: фрагменты вставленного кода (const …;, break;, }), stack-trace (at Foo (file:line:col)), лог-префиксы (app | …), одиночные символы. Причина — xterm при вставке шлёт текст как stdin, и quick-cmd.js считал каждую строку командой.

## Решение (три уровня)
1. **Источник — quick-cmd.js**: распознаём bracketed paste (ESC[200~ … ESC[201~). В CSI-парсере копим csiParams; 200~ → state.pasteMode=true, 201~ → false. В onPtyInput при pasteMode байты пропускаются. Новые вставки не засоряют forge.quickCmd.freq.
2. **Отображение — command-dock.js looksLikeCommand(s)**: эвристика прячет исторический мусор в computeFrequent(). Правила: длина ≥2; есть [A-Za-z0-9]; не оканчивается на ;{},= ; не начинается с ключевых слов ЯП; не stack-trace (^at … '('); не …file:line:col; не лог-префикс ^name | ; не начинается со скобки-закрывашки.
3. **Миграция — command-dock.js migrateFreqOnce()**: одноразовая физическая очистка forge.quickCmd.freq при старте (init). Версионный флаг forge.cmdDock.freqCleanV (FREQ_CLEAN_VERSION=1, повышать при улучшении эвристики). Удаляет ключи, не прошедшие looksLikeCommand, КРОМЕ закреплённых в forge.quickCmd.pinned и текстов собственных элементов дока (forge.cmdDock.items). Идемпотентна.

## Файлы
- tmux-web/static/quick-cmd.js — pasteMode + csiParams в state, обработка в processEscByte/onPtyInput
- tmux-web/static/command-dock.js — looksLikeCommand() в computeFrequent(); migrateFreqOnce() в init()
