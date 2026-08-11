# tmux-web/static/js/bitrix/mobile.js

Off-canvas колонка диалогов панели «Битрикс24» на узких экранах (Ф11).

На десктопе #b24-contacts — обычная колонка внутри #b24-body. При ширине <= 768px она уезжает за левый край (transform: translateX(-102%)) и выезжает поверх ленты по кнопке ☰; открытое состояние — класс b24-contacts-open на #bitrix.

КЛАСС НА #bitrix, А НЕ НА КОЛОНКЕ: одним состоянием управляются ТРИ элемента — сама колонка, подложка #b24-contacts-scrim и aria-expanded кнопки ☰. Общий предок избавляет от рассинхронизации.

ПОРОГ 768 ПРОДУБЛИРОВАН в css/bitrix.css (@media max-width: 768px). Иначе никак: CSS не читает JS-константы, а JS не должен парсить стили. Расхождение даёт худший из багов — колонка нарисована выезжающей, но closeB24ContactsAfterSelect считает экран широким и не закрывает её после выбора диалога. Комментарий-напоминание стоит в обоих местах.

ЗАКРЫТИЕ: выбор диалога (главный сценарий), клик по подложке, Esc, переход на десктопную ширину (MediaQueryList change — иначе класс остался бы висеть и подложка перекрыла бы широкую раскладку).

КАРТОЧКА ЗАДАЧИ на узком экране схлопнута до строки-шапки и раскрывается ТАПОМ (класс b24-task-card-open, max-height в CSS). Клики по КНОПКАМ карточки идут мимо переключателя: «завершил задачу и заодно свернул карточку» — не то, чего ждут.

Полная ширина ленты, safe-area-inset и 100dvh под экранную клавиатуру — правила CSS, а не состояние, и живут в css/bitrix.css рядом с медиа-запросом.

Экспорт: B24_MOBILE_MAX_WIDTH=768, B24_CONTACTS_OPEN_CLASS, B24_TASK_CARD_OPEN_CLASS, isB24NarrowScreen, isB24ContactsOpen, setB24ContactsOpen, closeB24Contacts, toggleB24Contacts, closeB24ContactsAfterSelect, isB24TaskCardOpen, setB24TaskCardOpen, initB24Mobile, _debugB24Mobile. E2E: сценарий «мобильная раскладка» в e2e/tests/ui/bitrix-ui.spec.ts.

