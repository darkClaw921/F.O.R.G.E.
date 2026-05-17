Phase 2: Разнос монолитного tmux-web/static/style.css (3419 строк / 83KB) на 17 фича-файлов в tmux-web/static/css/.

## Маппинг секций style.css → файлы

| Файл | Содержимое | Строки оригинала | Размер |
|------|-----------|------------------|--------|
| css/base.css | CSS-переменные тёмной темы + reset html/body/* | 1-128 | 6KB |
| css/layout.css | #layout grid, #main, #placeholder, .layout-collapsed, sidebar-toggle | 129-208 | 1.9KB |
| css/window-bar.css | tmux window tabs (внутри активной сессии) | 210-307 | 2.3KB |
| css/project-bar.css | #project-bar + .project-pill | 309-367 | 1.3KB |
| css/sidebar.css | .sidebar-header, .session-list, .session-item, .origin-group-header, .sidebar-footer, scrollbar | 369-516 + 572-724 | 6.3KB |
| css/origin-tabs.css | Phase 5 origin-табы над session-list | 517-570 | 1.2KB |
| css/tab-bar.css | Верхний #tab-bar (Terminal/Tasks/Git/Docker/Find) | 726-763 | 0.7KB |
| css/tasks.css | #tasks контейнер, .kanban-col/-card, DnD visual states, session-group-headers, TODO column | 765-963 + 1241-1391 | 8.8KB |
| css/modals.css | .modal-overlay, .modal-card, .modal-actions, Task modal (Phase 6.C) | 964-1240 | 5.5KB |
| css/notifications.css | Phase 5 Notifications form в settings (.notify-*) | 1393-1516 | 2.6KB |
| css/settings-modal.css | Settings tab-bar + Themes panel + Remote servers tab | 1518-1769 + 2709-2913 | 10.1KB |
| css/theme-editor.css | Custom theme editor modal (color-pickers + live preview) | 1770-2145 | 7.6KB |
| css/git-tab.css | #git pane + lazygit xterm | 2147-2350 | 4.8KB |
| css/tui-tab.css | Generic TUI containers (docker/telescope) + .tui-channel-bar | 2352-2572 | 5.0KB |
| css/tui-channels.css | (MISNOMER) — hotkey hints overlay, keyboard-focus, TODO plan-mode badge | 2574-2706 | 3.2KB |
| css/quick-cmd.css | Quick-cmd bar (mobile) + tui-quick-bar + edit UI modal + desktop fallback @media | 3198-3419 | 5.9KB |
| css/mobile.css | @media (max-width:768px) + @media (max-width:480px) + prefers-reduced-motion | 2914-3197 | 9.8KB |

## style.css теперь @import-агрегатор

3419 строк / 83KB → 67 строк / 2.7KB. Содержит только серию @import url('./css/...') в порядке оригинального каскада.

Порядок импорта (КРИТИЧЕН для каскада):
1. base — переменные и reset, ОБЯЗАТЕЛЬНО первым
2. layout
3. window-bar
4. project-bar
5. sidebar
6. origin-tabs (после sidebar, как в оригинале)
7. tab-bar
8. tasks
9. modals
10. notifications
11. settings-modal
12. theme-editor
13. git-tab
14. tui-tab
15. tui-channels
16. quick-cmd
17. mobile — ОБЯЗАТЕЛЬНО последним (перекрывает desktop через @media)

## Загрузка в браузере

index.html не менялся: <link rel=stylesheet href=/style.css>. Браузер тянет /style.css (67 строк), затем по @import — все 17 css/*.css. Все они embedded в бинарь через rust-embed (static_embed.rs), поэтому отдаются через axum-fallback без диска.

## Smoke OK

- cargo build green (warnings unrelated).
- devforge run --port 18765: все 17 /css/*.css endpoints → 200 + content-type: text/css. /style.css тоже 200 + text/css.
- Brace-count (открывающих {) идентичен: 470 = 470. Diff в строках = 13 (только пустые строки-разделители секций).

## Известные неточности плана (preserved 1:1)

- tui-channels.css по плану 2574-2706 называется 'telescope channel-bar', но реально в этих строках hotkey hints overlay + keyboard-focus + TODO plan-mode badge. Channel-bar на самом деле живёт в tui-tab.css (строки 2382-2572). Имя файла оставлено как в плане — refactor 1:1.
- settings-modal.css объединяет два разных диапазона (1518-1769 и 2709-2913) — оба логически принадлежат settings-модалу.
- tasks.css и modals.css имеют пересекающиеся диапазоны в плане; разделение по семантике: tasks = kanban-стили, modals = модалки.