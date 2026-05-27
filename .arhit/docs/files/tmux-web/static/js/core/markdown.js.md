# tmux-web/static/js/core/markdown.js

Утилита рендера markdown в DOM (tmux-web, vanilla JS). renderMarkdownInto(el, mdText) — парсит ограниченное подмножество markdown (заголовки ##, списки, абзацы) и безопасно (escape) вставляет в элемент. Используется страницей «Сводка дня» для отображения content отчёта.
