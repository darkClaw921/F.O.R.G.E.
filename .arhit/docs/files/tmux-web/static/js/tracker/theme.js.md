# tmux-web/static/js/tracker/theme.js

Собственная тема панели трекера — модуль тумблера ◐ (#trk-theme-toggle).

ЗАЧЕМ. Панель #tracker живёт в собственной терминальной палитре: все её цвета объявлены токенами --t-* на #tracker в css/tracker.css и НЕ наследуют токены приложения. Значит переключение темы devforge её не касается; на светлой теме приложения чёрная панель выглядела инородно, а тумблер в титлбаре был мёртв (кнопка есть с фазы 4, поведения не было).

ТРИ РЕЖИМА (localStorage 'forge.trackerTheme', по умолчанию 'auto'):
  auto  — светлота выводится из темы ПРИЛОЖЕНИЯ;
  dark  — всегда тёмная;
  light — всегда светлая.
Тумблер ходит по кругу auto → dark → light → auto.

КАК РАБОТАЕТ auto. Глобального флага «сейчас светлая тема» в devforge нет: js/themes/api.js::applyTheme просто расставляет CSS-переменные на :root. Поэтому светлота вычисляется из вычисленного значения --bg документа. Разбор цвета и яркость НЕ дублируются: берутся из общего слоя js/messenger/theme-core.js (resolveTheme, nextThemeMode, normalizeThemeMode, themeToggleTitle), покрытого Node-тестом. Имя каталога 'messenger' — след происхождения (первыми туда переехали Telegram и VK), а не ограничение: сущностей мессенджера в theme-core.js нет.

КАК ЛОВИТСЯ СМЕНА ТЕМЫ ПРИЛОЖЕНИЯ. applyTheme пишет переменные через root.style.setProperty, то есть меняет атрибут style у <html>. MutationObserver на attributeFilter ['style','class','data-theme'] — развязка, которая не тянет плагин в модуль тем (обратный импорт замкнул бы граф модулей). Наблюдатель пересчитывает тему только в режиме auto.

ГДЕ ХРАНИТСЯ РЕЖИМ. В приватном local модуля, а НЕ в состоянии панели — в отличие от VK, где режим лежит в vkState. Причина: у трекера общего состояния нет вовсе (js/tracker/main.js держит всё в приватном local) и main.js ГРУЗИТСЯ ЛЕНИВО, при первом открытии вкладки. Тема же обязана быть применена раньше: разметка #tracker есть в index.html всегда, и на светлой теме приложения панель мелькнула бы чёрной. Поэтому модуль автономен: статический импорт из bootstrap, ноль связей с main.js.

ПУБЛИЧНЫЙ API:
* TRACKER_THEME_STORAGE_KEY — ключ localStorage ('forge.trackerTheme');
* loadTrackerThemeMode() / saveTrackerThemeMode(mode) — чтение и запись режима; недоступный localStorage (приватный режим Safari) не ошибка, просто режим не переживёт перезагрузку;
* appBackgroundColor() — вычисленное --bg документа; пустая строка штатна, тогда resolveTheme берёт запасную тёмную;
* applyTrackerTheme() → 'dark'|'light' — ставит data-tracker-theme на #tracker и data-tracker-theme-mode + title/aria-label на кнопку; идемпотентна, DOM трогает только при реальном изменении;
* setTrackerThemeMode(mode) — явная установка (тумблер и e2e);
* cycleTrackerThemeMode() — следующий режим по кругу, это и делает клик по ◐;
* initTrackerTheme() — восстановить режим, навесить обработчик, поднять наблюдателя, применить; идемпотентна;
* teardownTrackerTheme() — снять наблюдателя (beforeunload / тесты);
* _debugTrackerTheme() — снимок {mode, applied, appBg, observing}.

ТОЧКИ ВЫЗОВА. initTrackerTheme — js/core/bootstrap.js (ДО initTrackerGate и НЕ завися от него: атрибут на скрытой панели бесплатен, а ждать ответа /api/tracker/status значило бы показать панель в чужой палитре). applyTrackerTheme — js/tabs/tabs.js на КАЖДОМ входе на вкладку (в auto пользователь мог сменить тему devforge, пока вкладка была закрыта), синхронно перед ленивым import панели. teardownTrackerTheme — beforeunload в bootstrap.

СВЯЗАННЫЙ CSS. css/tracker.css держит два полных набора токенов: тёмный на #tracker и светлый на #tracker[data-tracker-theme="light"]. Плюс два токена, которые раньше были литералами внутри правил и не переживали смену темы: --t-scrim (затемнение под модалками) и --t-drop (подсветка колонки-приёмника канбана). Правило #trk-theme-toggle[data-tracker-theme-mode="dark"|"light"] подсвечивает кнопку, когда тема зафиксирована явно.

РЕГРЕССИИ. tests/frontend/tracker_theme.test.mjs читает сам CSS и сверяет: наборы токенов тёмной и светлой палитр совпадают, весь текст даёт >= 4.5:1 к четырём фонам панели, рамка (--t-acc-faint) и приглушённый текст (--t-muted) разведены по ролям, скрим и подсветка приёмника разбираются как цвет.
