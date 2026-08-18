# installMeetPopups

Включение карточек встреч во всех трёх панелях — tmux-web/static/js/messenger/meet-popup.js.

Зовётся ОДИН РАЗ из core/bootstrap.js и только при 200 от GET /api/meet/status (рядом с connectMeetWs). При 404 (devforge без --meet) не зовётся никогда: ни одного узла, ни одного слушателя.

installMeetPopups({api, doc, onNotice}) запоминает конфигурацию и подставляет обработчик клика кнопке в лентах через configureMeetRow({open}). Экземпляр попапа создаётся ЛЕНИВО — при первом клике в конкретной панели (popupFor находит хост через doc.getElementById(panel)); панель, в которую человек не заходил, не платит ничем.

openMeetPopup(detail) — точка входа для клика/хоткея: выбирает экземпляр по detail.panel и открывает.
teardownMeetPopups() — снимает все карточки, зовётся в beforeunload bootstrap.js (слушатель Esc живёт на документе и переживает bfcache).
_debugMeetPopups() — снимок реестра для тестов: {installed, panels}.

api передаётся МОДУЛЕМ целиком (import * as meetApi), поэтому карточка зовёт только те функции, что объявлены в MEET_ROUTES; сверка имён — секция 6 tests/frontend/meet_api_gate.test.mjs.
