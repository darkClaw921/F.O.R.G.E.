# tmux-web/static/js/telegram/mobile.js

# js/telegram/mobile.js — off-canvas колонка контактов (Фаза 6)

## Что делает

На десктопе `#tg-contacts` — обычная колонка 322px внутри `#tg-body`. На ширине
<= 768px она уезжает за левый край (`transform: translateX(-100%)`) и выезжает
поверх ленты по кнопке ☰ `#tg-contacts-toggle`. Открытое состояние — класс
`tg-contacts-open` на `#telegram` (стили в css/telegram.css).

## Почему класс на #telegram, а не на самой колонке

Одним состоянием управляются ТРИ элемента: сама колонка, подложка
`#tg-contacts-scrim` и `aria-expanded` кнопки. Общий предок избавляет от
рассинхронизации.

## Закрытие

- выбор диалога — главный сценарий, ради него панель и открывали
  (вызов `closeTgContactsAfterSelect` из main.js::openDialogById; на десктопе no-op);
- клик по подложке `#tg-contacts-scrim`;
- Esc (обработчик на документе реагирует ТОЛЬКО при открытой панели, поэтому
  не конфликтует с Esc лайтбокса медиа);
- переход на десктопную ширину (matchMedia 'change') — иначе подложка осталась
  бы висеть поверх широкой раскладки.

## API

- `TG_MOBILE_MAX_WIDTH = 768` — порог, ПРОДУБЛИРОВАН в css/telegram.css;
  менять надо оба места (JS обязан знать, узкий ли экран, чтобы не закрывать
  несуществующую панель).
- `TG_CONTACTS_OPEN_CLASS = 'tg-contacts-open'`.
- `isTgNarrowScreen()`, `isTgContactsOpen()`.
- `setTgContactsOpen(open)` — переключает класс, `hidden` у подложки и
  `aria-expanded`; при открытии переносит фокус в колонку (у `<aside>` для
  этого стоит `tabindex=\"-1\"`).
- `closeTgContacts()`, `toggleTgContacts()`, `closeTgContactsAfterSelect()`.
- `initTelegramMobile()` — навесить обработчики; идемпотентна, зовётся из
  initTelegram().
- `_debugMobile()` — {narrow, open, scrimHidden} для e2e.

## Ограничения

- Модуль намеренно НЕ трогает `hidden` у `#tg-contacts`: колонка обязана
  оставаться в потоке, потому что на десктопе это единственный список диалогов.
  Скрывает её только transform.
- Сама кнопка ✈ живёт в `#settings-bar` внутри сайдбара devforge, а сайдбар на
  мобиле — off-canvas-ящик приложения. Чтобы попасть на вкладку с телефона,
  сначала открывают сайдбар: это существующее поведение devforge, плагин его
  не меняет.
