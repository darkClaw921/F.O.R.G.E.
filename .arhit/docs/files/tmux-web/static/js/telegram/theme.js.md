# tmux-web/static/js/telegram/theme.js

# js/telegram/theme.js — собственная тема вкладки Telegram (Фаза 6)

## Зачем модуль существует

Панель Telegram нарисована в терминальной палитре hablo-taste и НЕ наследует
токены приложения: все её цвета объявлены на `#telegram` в css/telegram.css.
Значит переключение темы devforge её не касается — и это правильно, пока
пользователь сидит в тёмной теме. Но на светлой теме приложения чёрная панель
выглядит инородно, поэтому у вкладки есть свой переключатель.

## Три режима

Хранятся в localStorage под ключом `forge.telegramTheme`, по умолчанию `auto`:

- `auto`  — светлота панели выводится из темы ПРИЛОЖЕНИЯ;
- `dark`  — всегда тёмная, тема приложения игнорируется;
- `light` — всегда светлая, тема приложения игнорируется.

Применение — атрибут `data-tg-theme` на `#telegram` (`dark` | `light`).
Светлые токены описаны селектором `#telegram[data-tg-theme=\"light\"]`.

## Как работает auto

Глобального флага «сейчас светлая тема» в devforge НЕТ: js/themes/api.js::applyTheme
просто расставляет CSS-переменные (`--bg` и прочие) на `:root`. Поэтому светлота
вычисляется из ВЫЧИСЛЕННОГО значения `--bg`:

  getComputedStyle(document.documentElement).getPropertyValue('--bg')
    -> parseCssColor -> relativeLuminance -> L > 0.5 ? light : dark

Разбор цвета и яркость — чистые функции в js/telegram/format.js (покрыты
Node-тестом tmux-web/tests/frontend/telegram_format.test.mjs), решение —
`resolveTgTheme`. Нераспознанный цвет (`oklch()`, `color()`, `transparent`)
даёт `TG_THEME_FALLBACK` = 'dark'.

## Как ловится смена темы приложения

`applyTheme` пишет переменные через `root.style.setProperty`, то есть меняет
атрибут `style` у `<html>`. MutationObserver на атрибуты `style`/`class`/`data-theme`
— единственная развязка, которая не тянет плагин в модуль тем: обратный импорт
telegram -> themes замкнул бы граф модулей в цикл. Наблюдатель пересчитывает
тему ТОЛЬКО в режиме auto.

## API

- `TG_THEME_STORAGE_KEY` — ключ localStorage ('forge.telegramTheme').
- `loadTgThemeMode(): 'auto'|'dark'|'light'` — читает режим; недоступный
  localStorage (Safari private mode) не ошибка, просто режим не переживёт
  перезагрузку.
- `saveTgThemeMode(mode)` — сохраняет.
- `appBackgroundColor(): string` — вычисленное `--bg` приложения.
- `applyTgTheme(): 'dark'|'light'` — применяет тему; идемпотентна и дешёвая,
  DOM трогается только при реальном изменении. Обновляет `title`/`aria-label`
  и атрибут `data-tg-theme-mode` у кнопки ◐.
- `setTgThemeMode(mode)` — ставит режим явно (используется e2e).
- `cycleTgThemeMode()` — то, что делает клик по ◐: auto -> dark -> light -> auto.
- `initTelegramTheme()` — восстановить режим, навесить тумблер, начать следить
  за темой приложения, применить. Идемпотентна.
- `teardownTelegramTheme()` — снять наблюдателя (beforeunload).
- `_debugTheme()` — снимок {mode, applied, appBg, observing} для e2e.

## Связи

- Зовётся из js/core/bootstrap.js (initTelegramTheme ДО initTelegramGate, чтобы
  вкладка не мигнула чужой палитрой) и js/tabs/tabs.js (applyTgTheme на КАЖДОМ
  входе на вкладку — в auto тему приложения могли сменить при закрытой панели).
- Читает/пишет tgState.themeMode (js/telegram/state.js).
- Ref'ы \$telegram и \$tgThemeToggle — js/core/dom.js.

## Ограничения

- В режимах dark/light тема приложения игнорируется полностью — это
  преднамеренно: пользователь зафиксировал вид панели явно.
- Кнопка ◐ доступна и до входа в аккаунт: titlebar лежит НАД оверлеем логина
  (z-index 6 против 5), иначе на светлой теме неавторизованная вкладка залипала
  бы в чужой палитре без возможности починить.
