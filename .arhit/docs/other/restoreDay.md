# restoreDay

tmux-web/static/js/home/home.js — восстанавливает ВСЕ сессии одного дня (группы) на экране «Недавние сессии». Экспортируемая функция, вызывается кнопкой .home-day-restore «Открыть все (N)» в заголовке группы-дня (см. buildDayGroup / renderHome). Принимает recs — массив записей HistorySession этого дня напрямую (не через мультивыбор selected).

Реализация: делегирует общему приватному хелперу restoreRecords(recs), который в цикле шлёт POST /api/sessions/history/restore {name, path} по каждой записи; 409 (имя занято) — мягкая ошибка, не прерывает цикл; возвращает {firstOk, failed}. После restoreDay обновляет список активных сессий (fetchSessions) и открывает первую успешно восстановленную (openSession firstOk); собранные ошибки — одним window.alert. Backend НЕ расширялся — переиспользуется существующий endpoint восстановления одной сессии (тот же, что у restoreSession/restoreSelected).

restoreRecords — общий хелпер, выделен из бывшего тела restoreSelected, чтобы restoreSelected и restoreDay не дублировали цикл восстановления. restoreSelected дополнительно чистит selected и updateSelectionUI; restoreDay этого не делает (у групп нет мультивыбора).
