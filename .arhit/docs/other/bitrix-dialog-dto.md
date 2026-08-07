# bitrix-dialog-dto

DialogDto и проекция строки списка диалогов Битрикс24 (plugins/bitrix/src/dto.rs, Ф4).

ЧТО ЭТО. Проводная структура одной строки колонки #b24-contacts плюс ЧИСТАЯ проекция сырья портала в неё (dialog_from_raw). Проекция не делает сетевых вызовов вообще: имена и присутствие приходят через trait PeerNames, поэтому вся таблица соответствий проверяется юнит-тестами на живых фикстурах без стенда и без вебхука.

ПОЛЯ. id (DIALOG_ID строкой), chat_id, kind, title, avatar_url (ТОЛЬКО путь прокси плагина), color, unread_count, last_message (LastMessageDto), date_update, pinned, muted, can_write, members, entity_type, entity_id, task_id, entity_link_url, role, online, was_online, archived (всегда false), folder_id (всегда null).

dialog_id ПРОТИВ chat_id — РАЗНЫЕ ПОЛЯ. dialog_id это адрес диалога в методах im.* ('chat7', 'sg12', '19'); chat_id — номер чата во внутренней таблице портала (7, 21, 3). У ЛИЧНОГО диалога dialog_id это user_id собеседника ('19'), а chat_id совсем другое число (21). Методы сообщений адресуются DIALOG_ID, методы чата (im.chat.*, im.disk.folder.get) — CHAT_ID. Разбор входящего идентификатора — parse_dialog_id: принимаются ровно три формы платформы (NNN, chatNNN, sgNNN), ведущий '+' отвергается, всё остальное даёт 404 unknown_dialog (а не 400: для фронта 'диалога нет' и 'id не разбирается' — одна ветка).

ПОЛИМОРФНЫЙ id. В im.recent.get поле id у группового чата строка ('chat7'), у личного — ЧИСЛО (19). Разбирается StrOrNum, наружу уходит ВСЕГДА строка: единая форма избавляет фронт от ветвления в ключах Map и в data-атрибутах. Строгое String-поле роняло бы разбор всего списка на первом же личном диалоге.

НАБОР kind: user | chat | task | sg | openline | copilot | collab. Определяет dialog_kind, порядок проверок значим: СНАЧАЛА задача (признак — entity_type == 'TASKS_TASK', а не тип чата), затем префикс sg, затем тип чата портала (tasksTask/copilot/collab/lines/open/sonetGroup), и только затем тип строки. Иначе чат задачи с type:'tasksTask' попал бы в обычные чаты, то есть исчез бы из директории ~/tasks. Незнакомый тип чата — обычный чат, а не ошибка.

НОРМАЛИЗАЦИЯ role. im.recent.get отдаёт 'OWNER', im.dialog.get — 'owner'; это один портал в ответах на соседние вызовы. normalize_role приводит к нижнему регистру ОДИН раз, здесь, чтобы сравнение role == 'owner' во фронте не зависело от того, каким методом приехала строка.

ЗАДАЧИ. task_id заполняется ТОЛЬКО когда entity_type == 'TASKS_TASK' (константа ENTITY_TASK; 'TASKS' без _TASK — другая сущность, на ней im.chat.get молча отдаёт null). entity_link_url — ОТНОСИТЕЛЬНЫЙ путь портала, не ссылка: хоста портала в DTO нет (он часть секрета), поэтому фронт использует поле как признак 'сущность есть', а не как href.

АВАТАРЫ (Р6). avatar_url — путь прокси /api/bitrix/media/avatar/<user_id>, никогда CDN-адрес портала, хотя тот и публичен. Только для личных диалогов (прокси адресуется user_id) и только когда аватар реально есть: портал вместо 'нет аватара' присылает три разных значения — false, пустую строку и заглушку blank.gif (то относительным, то абсолютным адресом).

ПОЛЯ-КОНСТАНТЫ archived=false и folder_id=null. У портала нет ни архива, ни папок. Поля сохранены ради ИДЕНТИЧНОСТИ ФОРМЫ с VK и Telegram: каждое удалённое поле превратилось бы в if во фронте, а построчный дифф трёх панелей — главный инструмент их сопровождения.

ПРОЧЕЕ. muted — это СВОЙ id в chat.mute_list, а не непустой список (непустой означает лишь, что кто-то другой не хочет уведомлений). can_write — конъюнкция text_field_enabled и restrictions.send. last_message — отдельный тип LastMessageDto, а не MessageDto (Ф5): портал отдаёт в recent[].message ДРУГОЙ объект без params, вложений и клавиатур; текст прогоняется через bbcode::to_plain и обрезается до PREVIEW_CHARS=160. merge_dialogs склеивает два источника списка без дублей, приоритет у im.recent.get (Р3).

СВЯЗИ: api/schema.rs (RecentItemRaw), bbcode.rs (to_plain), peers.rs (реализация PeerNames), routes/dialogs.rs (потребитель), ws/protocol.rs (DialogPayload).
