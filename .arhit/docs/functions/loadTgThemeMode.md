# loadTgThemeMode

Читает режим темы панели Telegram из localStorage и нормализует его. Недоступный localStorage (Safari private mode, отключённые куки) — не ошибка: возвращается 'auto', просто режим не переживёт перезагрузку. См. js/telegram/theme.js.
