# frameFlag

tmux-web/static/js/telegram/main.js — значение флага WS-кадра с откатом на локальное знание.

Сигнатура: frameFlag(value, fallback) -> boolean.

Правило одно: если typeof value === 'boolean' — берётся value, иначе fallback. Не-boolean
означает «поля нет» (старый сервер), и это НЕ то же самое, что false.

Ради этой единственной строчки функция и выделена: наивное !frame.muted превратило бы «не знаю»
в «не заглушён» и вернуло бы дефект D1. Используется в dialogRings и rememberFrameFlags.
