# bitrix-dialogs-routes

РОУТЫ ДАННЫХ И ДЕЙСТВИЙ ПАНЕЛИ (plugins/bitrix/src/routes/dialogs.rs).

ЧТО СМОНТИРОВАНО.
| метод | путь | фаза |
|---|---|---|
| GET | /api/bitrix/filters | Ф4 — четыре директории одного списка + счётчики (Р2) |
| GET | /api/bitrix/dialogs?limit&offset&filter | Ф4 — страница списка (Р3: im.recent.get + дозагрузка im.recent.list) |
| GET | /api/bitrix/dialogs/:id/messages?limit&before_id&after_id | Ф5 — страница истории |
| GET | /api/bitrix/messages/:mid?dialog_id | Ф5 — одно сообщение (догрузка realtime, Ф7) |
| POST | /api/bitrix/dialogs/:id/messages | Ф6 — отправка (Р9, Р10, Р12) → bitrix-send-message |
| PATCH | /api/bitrix/messages/:mid | Ф6 — правка → bitrix-like-pin-hide |
| DELETE | /api/bitrix/messages/:mid | Ф6 — удаление → bitrix-like-pin-hide |
| POST | /api/bitrix/messages/:mid/like | Ф6 — «лайк» → bitrix-like-pin-hide |
| POST | /api/bitrix/dialogs/:id/read | Ф6 — отметка о прочтении (окно 800 мс) → bitrix-read-typing |
| POST | /api/bitrix/dialogs/:id/unread | Ф6 — ручное «непрочитано» (message_id ОБЯЗАТЕЛЕН) → bitrix-read-typing |
| POST | /api/bitrix/dialogs/:id/typing | Ф6 — «печатает…» (окно 8 с) → bitrix-read-typing |
| POST | /api/bitrix/dialogs/:id/pin | Ф6 — закрепить/открепить (Р14) → bitrix-like-pin-hide |
| POST | /api/bitrix/dialogs/:id/hide | Ф6 — скрыть из списка (Р14) → bitrix-like-pin-hide |

ЛЕСТНИЦА ПРОВЕРОК 409 → 503 → 404 → 400, порядок ЖЁСТКИЙ во всех data-роутах:
1. 409 not_authorized — вебхук не подключён (require_authorized);
2. 503 not_connected — портал недоступен прямо сейчас (require_connected по ConnHealth);
3. 404 unknown_dialog / unknown_message — объект не резолвится (require_dialog_id → dto::parse_dialog_id, require_message_id → положительное целое);
4. 400 invalid_input — и только теперь разбор query и ТЕЛА.
Смысл порядка: враждебный ввод не должен доходить до парсера, пока не пройдена авторизация, иначе роут превращается в ОРАКУЛ — по коду ответа неподключённый клиент узнаёт, какие идентификаторы портал считает валидными. Отсюда два следствия: query разбирается ВРУЧНУЮ (query_i64/query_str), а тело — через Option<Json<T>> (типизированные экстракторы axum отработали бы ДО тела хендлера и вернули 400 раньше 409, сломав лестницу молча — сборка при этом не упала бы). Стражи: tests/dialogs_ladder.rs (hostile_dialog_ids_hit_auth_before_any_parsing, a_broken_connection_answers_before_the_parser, an_unparseable_dialog_id_answers_404_before_the_parser, garbage_history_parameters_are_rejected_after_the_dialog_resolves) и tests/b24_stub.rs::the_write_routes_keep_the_check_ladder (девять пишущих роутов на неподключённом плагине дают 409 и НОЛЬ сетевых вызовов).
Враждебная строка НИКОГДА не цитируется обратно в сообщении ошибки.

ИСТОРИЯ СООБЩЕНИЙ (Ф5).
- limit клампится 1..=50 (потолок платформы) МОЛЧА;
- before_id → LAST_ID (строго старее, листаем вверх), after_id → FIRST_ID (строго новее, догружаем низ); обе границы разом трактовать нечем, поэтому побеждает before_id;
- ПОРЯДОК ОТВЕТА ПОРТАЛА РАЗНЫЙ (убывание на базовом вызове и LAST_ID, возрастание на FIRST_ID) — на выход ВСЕГДА возрастание (dto::sort_messages_ascending);
- has_more считается как «страница пришла полной»: точного признака у метода нет (ни total, ни hasMore), и цена ошибки — один лишний запрос при прокрутке, а не потерянные сообщения;
- load_page — ОБЩАЯ функция обоих ленточных роутов И перечитывания строки после действия (Ф6): поглощает users[] (живьём чаще всего пуст), добирает неизвестных авторов ОДНИМ batch (ошибка добора не роняет страницу — подписью станет идентификатор), строит индекс files[] и проецирует сообщения;
- GET /messages/:mid просит страницу на ОДНУ строку с LAST_ID = mid+1: метода «дай сообщение по идентификатору» у портала нет вовсе, а идентификаторы растут шагом 2, поэтому между mid и mid+1 чужого сообщения быть не может. Если вернулся сосед — 404 unknown_message.

СПИСОК ДИАЛОГОВ (Ф4). Директории all/chats/tasks/unread — НЕ серверные фильтры и не папки: портал отдаёт один список, разделение выводится из полей строки (dir_matches — построчная копия фронтового dirMatches). Сортировка серверная: пины сверху, дальше по date_update, иначе вторая страница содержала бы строки первой.

ФОРМЫ ОТВЕТОВ ДЕЙСТВИЙ (Ф6). Два типа на девять роутов:
- MessageResp {id, message, echoed} — отправка, правка, лайк. Поле echoed отвечает на вопрос «строка перечитана у портала или собрана локально»: у перечитанной канонический текст (URL_PREVIEW переписывает ссылки), настоящая метка правки и настоящий состав лайков. echoed:false НЕ означает неуспех — действие выполнено, не удалось только перечитывание.
- AckResp {ok, sent} — удаление, read, unread, typing, pin, hide. sent:false бывает только у read и typing и означает «подавлено окном троттлинга» (bitrix-read-typing), а не ошибку.

ЦЕНА В ЗАПРОСАХ. Список: один batch (Р3) + при необходимости дозагрузка и один batch добора имён. История: один вызов + при необходимости один batch добора имён. Отправка/правка/лайк: ДВА вызова (действие + перечитывание строки). Прочтение и «печатает…»: не больше одного вызова в окно. Бюджет портала — 2 запроса в секунду на пару портал+IP.
