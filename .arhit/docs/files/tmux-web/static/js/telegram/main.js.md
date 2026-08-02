# tmux-web/static/js/telegram/main.js

Оркестратор вкладки Telegram (обновлено в Фазе 3).

initTelegramGate() — гейт кнопки ✈ по GET /api/telegram/status (404 = плагин выключен, молча). С Фазы 3 через applyLoginState здесь же поднимается ПОСТОЯННЫЙ WS, если аккаунт авторизован — не дожидаясь открытия вкладки, иначе бейдж и тосты появлялись бы только после первого захода в Telegram.

buildHandlers() — карта 13 типов кадров: open/close/lat/resync + login_state/new_message/message_edited/message_deleted/typing/online/read + upload_progress/download_progress (no-op под Фазы 4-5, чтобы новые события не сыпали 'no handler').

recomputeUnread() — ЕДИНСТВЕННЫЙ путь обновления #tg-unread-badge: события сначала правят unread_count в tgState.dialogs, потом идёт пересчёт суммы по немьюченным (два независимых счётчика неизбежно разъезжаются).

Тосты: notify() из echo/notifications.js; только чужие сообщения, только когда вкладка не активна или открыт другой диалог, троттлинг 5с на диалог; клик -> клик по #telegram-toggle (переключение вкладки БЕЗ импорта tabs.js — иначе цикл модулей) + openDialogById.

typing: показ + автосброс 6с (Telegram не присылает 'перестал печатать'). suspendTelegram (visibilitychange:hidden) закрывает WS и гасит таймеры; resumeTelegram перечитывает статус; teardownTelegram добавляет revokeObjectURL через clearTgAccountData. applyLoginState при разлогине рвёт WS и чистит колонку/ленту, при входе — поднимает WS и грузит диалоги.
