# fmtAxisTime

Форматирует мс в строку времени HH:MM через new Date(ms).toLocaleTimeString(undefined,{hour:'2-digit',minute:'2-digit'}), обёрнуто в try/catch (на невалидной дате возвращает ''). Используется buildAxis для подписей засечек оси при узких дневных диапазонах (span<=2*DAY_MS: 'today'/'yesterday'). Парная функция к fmtAxisDate (даты для широких окон).
