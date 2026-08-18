# meet-popup.js

Карточка встречи (попап), общая на три панели — tmux-web/static/js/messenger/meet-popup.js. Фронтовая Ф4 forge-meet.

Экспорт: createMeetPopup(cfg) — фабрика по экземпляру на панель; installMeetPopups/openMeetPopup/teardownMeetPopups/_debugMeetPopups — реестр и подключение из bootstrap; formatClockTime(ms); константы MEET_POPUP_CLASS, MEET_TABS, TRANSCRIPT_PAGE (500), TRANSCRIPT_CAP (2000).

Импортирует: createFollow (follow-core.js), renderMarkdownText (markdown.js), configureMeetRow/formatMeetClock/meetViewFor (meet-row.js), читатели стора (meet-store.js). Сеть — инъекцией через cfg.api, поэтому модуль грузится в голом Node.

КЛЮЧЕВЫЕ РЕШЕНИЯ (подробности в шапке файла):
1. По экземпляру на панель — иначе токены палитры не резолвятся (они объявлены на #telegram/#vk/#bitrix).
2. Разметка узлами, без innerHTML.
3. Живая расшифровка: дельты копятся в буфере и применяются ОДИН раз за кадр rAF; автопрокрутка общим createFollow с замером «у нижнего края» ДО мутации DOM; виртуализации нет — потолок 2000 строк со срезом начала и пагинация страницами по 500.
4. Плеер только через fetch -> blob: -> objectURL с обязательным revokeObjectURL (у <audio> нет Bearer-токена; архив ~14 МБ на час).
5. Саммари — общим renderMarkdownText; провал summary_status=failed показывается текстом и кнопкой retry?from=summarizing.
6. Гейт keynav: panelVisible() всех трёх панелей ищет .meet-popup внутри своей панели (класса .modal-overlay попап не несёт намеренно).

Тесты: tests/frontend/meet_popup*.test.mjs, ветки keynav — в telegram_keynav/vk_keynav/bitrix_keys.

ЖИВАЯ ПРИЁМКА (гейт forge-meet-fe4-9eamn.5, 2026-08-17): карточка проверена в настоящем Chrome по CDP — три панели, две темы, палитра без единого чужого цвета, ветка panelVisible настоящими клавишами, наложение поверх попапа анализа, настоящий 10-минутный файл с перемоткой. 113 проверок, 0 провалов. Подробности и грабли пробы — страница meet-popup-live-probe. Ф11: таблица действий ACTS_BY_STATUS ЭКСПОРТИРОВАНА (её сверяет контрактный тест с MeetStatus::ALL в обе стороны) и получила строку pruned — вычищенной ретенцией встрече предлагается ровно одно действие «ЗАПИСАТЬ», а не набор псевдостатуса none.
