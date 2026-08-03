# LpCursor

plugins/vk/src/state.rs::LpCursor — курсор LongPoll ВКонтакте:
{server, key, ts, pts}.

server/key приходят от messages.getLongPollServer, ts — внутренний счётчик
событий сервера (НЕ unix-время), pts — курсор истории для getLongPollHistory
(появляется только если LP_MODE несёт бит 32).

ПОЧЕМУ ХРАНИТСЯ С ОТМЕТКОЙ ВРЕМЕНИ. На диске лежит не голый LpCursor, а
PersistedCursor{cursor, saved_at}: холодного catch-up у VK нет (код 10 на ts
старше суток), а по самому ts понять, протух он или нет, НЕВОЗМОЖНО — это
счётчик, а не время. Без отметки записи плагин после долгого простоя либо
терял бы события молча, либо запрашивал заведомо мёртвый курсор.

ЖИЗНЬ. Живёт в VkState::lp_cursor (RwLock) и переживает рестарт процесса.
Обновляется каждым успешным a_check; при failed:1 берётся ts из ответа, при
failed:2 — НЕТ (см. recovery_for), при failed:3 сбрасывается целиком.
