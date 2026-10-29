# tmux-web/static/js/bitrix/portals.js

Строка вкладок порталов панели «Битрикс24» (мультипортальность, Ф8). Отдельный ряд #b24-portals под #b24-titlebar: вкладка на каждый подключённый портал, бейдж непрочитанного, кнопка + и контекстное меню «переименовать / отвязать». Ряд виден ВСЕГДА, включая мобильную раскладку, где колонка #b24-contacts уезжает под ☰ — на узком экране вкладка портала единственный индикатор того, в чей чат уйдёт сообщение.

ЧИСТАЯ ЛОГИКА (без DOM, покрыта tests/frontend/bitrix_portals.test.mjs):
- portalLabel(p) — подпись вкладки; ПУСТОЙ title показывается как id (хост портала бэкенд не отдаёт вовсе, узнаваемое имя несёт только id);
- truncateLabel(text, max=TAB_LABEL_MAX=18) — усечение под ширину вкладки;
- computeTabs(portals, activeId, unreadMap) — состояние вкладок: {id,label,title,unread,active,connected,state}; живой счётчик перекрывает снимок PortalDto.unread;
- nextActiveId(portals, wantedId) — остаться на прежнем -> первый в реестре -> пусто;
- orderOf(portals) — порядок вкладок списком id (то, что уезжает в PATCH);
- movedOrder(ids, dragged, target) — новый порядок после перетаскивания.

СОСТОЯНИЕ И ПУБЛИЧНЫЙ API: activePortalId, portalTitle, hasPortals, portalsList, setPortalUnread (точное число из кадра counters), bumpPortalUnread (приращение на message_new соседа), rememberDialog/rememberedDialog (последний открытый диалог КАЖДОГО портала), loadPortals (GET /api/bitrix/portals), applyPortalsFrame (WS-кадр portals), switchPortal, renamePortal, removePortal, handleUnknownPortal, render, closeMenu, initB24Portals, resetPortals, _debugPortalsState.

ЗАВИСИМОСТИ: core/dom.js ($b24Portals, $b24PortalTabs, $b24PortalAdd, $b24PortalMenu, $b24PortalRename, $b24PortalRemove), bitrix/api.js (CRUD порталов и setActivePortal), bitrix/state.js (читает activeDialogId). Модуль НЕ импортирует main.js/dialogs.js/chat.js: reset панели и загрузка данных приходят колбэком onSwitch(id, rememberedDialogId) — прямой импорт замкнул бы цикл.

ЗАФИКСИРОВАННЫЕ РЕШЕНИЯ: пустой title = id; portal_id уходит на сервер дословно; пустой массив вкладок означает «порталов ещё нет» (слоты без вебхука сервер фильтрует сам, реестр пустым не бывает); перестановка шлёт ПОЛНЫЙ список id, а не позицию; отвязка спрашивает дважды (сама отвязка обратима, снос кэша нет); реакция на 404 unknown_portal ровно одна — перечитать состав и переключиться. Только createElement/textContent, innerHTML запрещён.
