# connectTrackerWs

Сокет панели трекера (tmux-web/static/js/tracker/ws.js).

Открывает /ws/tracker, первым кадром получает снимок и дальше принимает кадры мутаций (task_upsert, task_delete, entry_*, item_*, list_*, milestone_*, reload). В отличие от мессенджеров сокет НЕ постоянный: он живёт, только пока открыта вкладка трекера, и закрывается при уходе (disconnectTrackerWs) — бейджа непрочитанного у трекера нет, задачи извне не приходят. Разбор кадра вынесен в чистую frameToHandler и покрыт node-тестом; состояние соединения видно из _debugWsState(), статусбар показывает ONLINE/OFFLINE.
