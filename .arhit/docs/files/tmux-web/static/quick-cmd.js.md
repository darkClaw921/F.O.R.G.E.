# tmux-web/static/quick-cmd.js

Frontend модуль quick-cmd.js: панель быстрых команд для mobile (tmux-web/static/quick-cmd.js).

## Назначение
IIFE-модуль, изолированный от app.js по образцу hotkeys.js. Рендерит #quick-cmd-bar внутри #terminal и три #*-quick-bar внутри TUI-вкладок (#git/#docker/#telescope), видимые только на mobile (matchMedia '(max-width: 768px)'). Авто-трекит частоту команд из stdin и предоставляет top-N кнопок: тап → отправка команды в активный PTY через window.ForgeApp.sendToActivePty.

## Public API
- window.QuickCmd.onPtyInput(data: string) — hook из app.js term.onData; буферизует ввод пользователя до \r/\n и считает частоту.
- window.QuickCmd.openEditor() — открывает модалку редактирования списка.
- window.QuickCmd.refresh() — перерисовать #quick-cmd-bar (вызывать после внешних изменений).
- window.QuickCmd.refreshTuiBars() — перерисовать TUI bars.

## Константы и LS-схема
- LS_KEY_FREQ='forge.quickCmd.freq' — { [cmd]: count } (счётчик частоты).
- LS_KEY_PINNED='forge.quickCmd.pinned' — string[] (закреплённые команды, всегда показываются).
- LS_KEY_HIDDEN='forge.quickCmd.hidden' — string[] (скрытые из авто-списка).
- TOP_N=8.
- DEFAULTS=['ls','cd ..','git status','clear','exit'] — стартовый набор.

## Фильтрация мусорного ANSI-эха (важно!)
xterm.term.onData срабатывает не только на пользовательский ввод, но и на ПРОГРАММНЫЕ ответы xterm на запросы программы — Device Attributes (\x1b[?64;1c), color queries OSC 10/11/12 (\x1b]11;rgb:RRRR/GGGG/BBBB\x07), cursor position и т.п. Без фильтра фрагменты этих ответов оседают в буфере и записываются как 'команды' (ранее в bar появлялись '0A', ']11;rgb:4c4c/4f4f/69...').

Двухуровневая защита:
1. **Расширенный ESC-skip в onPtyInput**: при \x1b сбрасываем буфер и переходим в state.escMode. Поддерживаются CSI (\x1b[ ... final-byte 0x40-0x7e), OSC (\x1b] ... BEL 0x07 или ST), DCS (\x1bP), APC (\x1b_), PM (\x1b^), SOS (\x1bX), SS3 (\x1bO + 1 byte). escMode сохраняется между вызовами onPtyInput — xterm может фрагментировать data. Реализация: state.escMode + processEscByte(code, ch) с переходами 'init'→'csi'/'osc'/'dcs'/.../'st-tail'/null.
2. **normalize() с regex-фильтрами ANSI_ECHO_PATTERNS**: отбрасывает строки, начинающиеся с [,],?,O/o; содержащие 'rgb:[0-9a-f]'; матчащие CSI-параметры '^\d+(;\d+)*[a-zA-Z~]?$'; заканчивающиеся на \; содержащие ';[0-9a-f]{4}/' (rgb-фрагмент). Также reject управляющих байт <0x20 (кроме \t), DEL 0x7f, length>200.

## Миграция localStorage
При init() вызывается migrateStorage() — проходит по freq/pinned/hidden и удаляет ключи, не прошедшие текущий normalize(). Это очищает мусор, накопленный предыдущей (более слабой) версией фильтра. Безопасно: применяется к каждой загрузке страницы, но удаляет только невалидные команды.

## Алгоритм onPtyInput
1. Если state.escMode активен → processEscByte() и continue.
2. \x1b → startEsc() (сброс буфера, mode='init').
3. \r/\n → команда = normalize(buffer), buffer='', если валидна — bumpFreq + refresh (на mobile).
4. 0x7f/0x08 (Backspace/DEL) → buffer.slice(0,-1).
5. 0x03/0x04 (Ctrl+C/D) → buffer=''.
6. 0x20-0x7e (печатный ASCII) → buffer += ch.
7. >0x7f (UTF-8 байты) → buffer += ch (но не 0x9c — 8-bit ST).
8. прочие управляющие — игнор.

## Рендеринг #quick-cmd-bar
Структура HTML: .quick-cmd-keys (spec-keys: Esc=\x1b, Tab=\t, ^C=\x03, стрелки CSI) + .quick-cmd-cmds (top-N команд) + кнопка ✎ (openEditor). Tap по команде → sendToPty(cmd + '\r'). Tap по spec-key → sendToPty(raw bytes). isMobile() гейтит видимость.

computeTopCommands(): pool = pinned ∪ freq-keys ∪ DEFAULTS; фильтр hidden (кроме pinned); сортировка [pinned-order, -freq, DEFAULTS-order, alpha]; slice(0, TOP_N).

## TUI quick-bars
Каждой TUI-вкладке (git/docker/telescope) соответствует свой #*-quick-bar div. Набор TUI_KEYS: [q, Esc, ?, :, /, h, j, k, l, Enter, ^C, Tab, ↑↓←→]. MutationObserver на атрибуте hidden панелей #git/#docker/#telescope синхронизирует видимость bar'а с activeTab. Tap → ForgeApp.sendToActivePty — sendToActivePty сам выберет нужный WS (state.gitTerm.ws / state.dockerTerm.ws / state.telescopeTerm.ws) по state.activeTab.

## Edit-UI
Модалка #quick-cmd-editor (lazy build): + Add input, список всех известных команд с per-row кнопками 📌(pin/unpin) / 🚫👁(hide/show) / 🗑(delete полностью). Все изменения → localStorage + refresh().

## Зависимости
- window.ForgeApp.sendToActivePty (экспорт из app.js)
- xterm term.onData hook в app.js (call window.QuickCmd.onPtyInput внутри обоих term.onData колбэков — main и createTuiTab)
- localStorage (3 ключа)
- matchMedia '(max-width: 768px)' для mobile-only видимости

## Ограничения
- onData ловит все байты, включая программные responses терминала. Двухуровневая фильтрация (escMode + normalize regex) снимает 99% мусора, но edge-cases возможны если paste содержит ANSI escape posуществу совпадающие с pattern.
- Команды '0A' и подобные короткие digit+letter отбрасываются (приоритет — чистота списка). Если такая команда нужна — можно добавить вручную через Edit UI и закрепить (pinned обходит фильтр freq, но не normalize в input pipeline; вручную добавленная команда через openEditor проходит normalize только при ручном add — фактически такие крайние случаи дойдут до freq только если пользователь явно добавит).
