# plugins/vk/src/api/methods.rs

Типизированные обёртки методов VK: собрать Params, позвать VkClient::call, разобрать в schema.rs. Ни сети, ни ретраев, ни троттлинга здесь нет.

ПРАВИЛА МОДУЛЯ.
1. access_token в Params не кладётся НИКОГДА (его добавляет транспорт) — тест params_never_contain_the_token проверяет отладочный вывод, именно он попал бы в лог.
2. Каждый мутирующий метод принимает необязательную Captcha{sid,key} и подставляет captcha_sid/captcha_key как есть: повтор с фронта = тот же запрос плюс два поля. Общего авто-решателя captcha нет и не планируется.
3. random_id ВСЕГДА приходит снаружи (new_random_id): VK дедуплицирует messages.send по нему и на повтор возвращает ТОТ ЖЕ message_id. Это делает безопасным автоповтор мутирующего метода — при условии, что random_id сгенерирован один раз на логическую отправку.
4. Границы страниц зажимает clamp_limit.

КОНСТАНТЫ. MAX_DIALOGS = 100 (в документации VK 200, на практике меньше; свой потолок заведомо ниже любого наблюдаемого), MAX_MESSAGE_IDS = 100, USER_FIELDS, GROUP_FIELDS, EXTENDED_FIELDS, LP_VERSION = 3, DIALOG_FILTERS = [all, unread, important, unanswered].

ЧИСТЫЕ ФУНКЦИИ. clamp_limit(i64) -> i64 в 1..=100 (ноль и отрицательные -> 1: ?limit=0 это опечатка клиента, молча отдать сотню значило бы скрыть ошибку). normalize_filter(&str) -> &'static str, незнакомое -> all. new_random_id() -> i32 положительный из младших бит Uuid v4 (ноль исключён: у VK random_id=0 означает «дедупликацию не делать»); крейт rand не заводится. array_or_field(Value, key) переживает обе формы ответа VK (голый массив у users.get и объект {groups:[...]} у groups.getById с v5.147).

МЕТОДЫ (impl VkClient): users_get (пустой список = «расскажи о владельце токена», это проверка токена в Ф3), groups_get_by_id (принимает ПОЛОЖИТЕЛЬНЫЕ id — знак снимает вызывающий), get_conversations (extended=1 обязателен: profiles и groups приезжают вместе с беседами, иначе втрое быстрее упирались бы в лимит), get_history (start_message_id + offset=1 — способ VK сказать «то, что старше», сам start в ответ не попадает), get_by_id (пустой items это НЕ ошибка транспорта: превращение в 404 unknown_message живёт в роутах Ф4), send_message(peer_id, random_id, SendBody) -> i32, mark_as_read, set_activity (вид нормализуется: незнакомый VK отвергает кодом 100), get_longpoll_server (need_pts=1 обязателен, иначе не вызвать getLongPollHistory), get_longpoll_history (preview_length=0 — урезанный текст выглядит как повреждённые данные), photos_upload_server/photos_save, docs_upload_server/docs_save.

SendBody { message, attachment, reply_to, payload, captcha } — структура, а не семь аргументов: позиционный вызов на семь Option нечитаем.
