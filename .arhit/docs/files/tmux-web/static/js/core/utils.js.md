# tmux-web/static/js/core/utils.js

Pure helpers — экранирование, modal-overlay, clipboard, detectClientOS.

## Назначение
1:1 копии utility-функций из IIFE `tmux-web/static/app.js`. Pure helpers — без side-effects (кроме clipboard, который ходит в DOM).

## Экспорты
### Экранирование строк
- `escapeHtml(s): string` (app.js:4408) — экранирует & < > " ' для безопасной вставки в innerHTML (полная версия). Null/undefined → пустая строка.
- `escapeAttr(s): string` (app.js:5852) — облегчённое экранирование для attribute values (& " <).
- `escapeText(s): string` (app.js:5855) — экранирование text content (& < >).

### Modal builder
- `buildModalOverlay(): HTMLDivElement` (app.js:5582) — создаёт пустой `<div class="modal-overlay">`. CSS — в `style.css` секция Modals.

### Client detection
- `detectClientOS(): 'mac'|'windows'|'linux'|null` (app.js:2019) — определение ОС по `navigator.platform` + `userAgent`. Используется для подсказок hotkey-ов (Cmd vs Ctrl).

### Clipboard
- `copyToClipboardSafe(text: string): Promise<boolean>` (app.js:2034) — копирует строку через Clipboard API с fallback на `document.execCommand('copy')`.
- `fallbackCopy(text: string): boolean` (app.js:2041) — синхронный textarea + execCommand fallback. Возвращает true при успехе.

## Зависимости
НЕТ — pure helpers (только браузерные навигатор/clipboard/document).

## Использование (Phase 1+)
Импортируется из feature-модулей `tasks/*`, `settings/*`, `sidebar/*`, `projects/*`, `themes/*` для рендера и UI.

## Ограничения
- escapeHtml/escapeAttr/escapeText имеют разные наборы заменяемых символов — намеренно (полная версия для HTML, облегчённая для attr/text). Не унифицировать без необходимости.
- fallbackCopy зависит от deprecated `document.execCommand` — оставлен как fallback для старых браузеров.
