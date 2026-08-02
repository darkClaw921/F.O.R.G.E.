# base64_encode

plugins/telegram/src/dto.rs — кодирование callback-data в base64 (standard alphabet, С padding).

Сигнатура: base64_encode(bytes: &[u8]) -> String. ~40 строк без зависимостей.

ЗАЧЕМ СВОЙ, А НЕ КРЕЙТ base64. Обоснование зафиксировано комментарием в коде: base64 0.22 уже
лежит в Cargo.lock (тянется tmux-web), то есть выигрыш «крейт уже собран» РАВЕН НУЛЮ, а плата —
лишняя строка в манифесте плагина и ещё одна поверхность обновлений. Операций всего две
(encode при отдаче DTO, decode при приёме callback), над блобами ≤64 байт, оба направления
покрыты тестами.

Используется в reply_button_dto для ReplyButtonDto::data: callback_data у Telegram — произвольные
БАЙТЫ, не UTF-8, и в JSON-строку их иначе не положить.

Обратная операция — base64_decode.
