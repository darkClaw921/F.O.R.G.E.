# plugins/telegram/src/dto.rs

Wire-DTO плагина Telegram. Фаза 3 (в Фазе 1 была только заглушка MessageDto).

Типы: DialogDto{id,kind(user|group|channel),title,username,unread_count,pinned,last_message}, MessageDto{id,dialog_id,out,date,sender,text,media,reply_to_id,edit_date}, MediaDto{kind,mime,size,file_name,width,height,duration_secs,has_thumb}.

ДВА ИНВАРИАНТА: (1) любой 64-битный id — СТРОКА в Bot API-формате (JS теряет точность выше 2^53); message_id остаётся числом (i32 в Telegram). (2) Option-поля сериализуются ЯВНЫМ null — skip_serializing_if в DTO НЕ используется, чтобы фронт отличал 'поля нет в контракте' от 'значения нет'.

Мапперы (сети не касаются, безопасны в hot-path апдейтов): from_grammers_dialog (unread_count/pinned берутся из сырого tl::enums::Dialog — геттеров у grammers нет), from_grammers_message, media_dto (WebPage -> None: превью ссылки не вложение; прочее -> other). Хелперы: dialog_id_string/parse_dialog_id/kind_of/peer_kind/peer_title/sender_dto/document_kind.

document_kind классифицирует по АТРИБУТАМ документа, MIME только фолбэк: у голосового MIME audio/ogg, и без флага voice оно выглядело бы обычной музыкой; стикер важнее video-атрибута; GIF (animated) — видео-карточка.
