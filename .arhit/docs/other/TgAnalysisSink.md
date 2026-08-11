# TgAnalysisSink

Мост между общим ядром анализа чата (forge_chat_analysis::AnalysisSink) и broadcast-каналом плагина Telegram. Живёт в plugins/telegram/src/ws/protocol.rs рядом с enum TgServerMsg — там же, где придётся править, если состав полей кадра изменится.

Вся развязка плагина с крейтом — четыре метода: started/delta/done/error. Ядро не знает ни про TgServerMsg, ни про broadcast; оно зовёт трейт, а перевод в кадр плагина и рассылка живут здесь.

Методы СИНХРОННЫЕ намеренно: их зовёт пампер ядра из своей таски, в том числе в горячем цикле дельт. broadcast::Sender::send — мгновенная неблокирующая операция, поэтому пампер не стоит и буфер коалесцинга не растёт. Сделать методы async значило бы разрешить реализации ждать внутри пампера.

SendError глотается: отсутствие подписчиков — норма (никто не открыл вкладку), и это не повод ронять уже идущий run.

Создаётся через TelegramState::analysis_sink(), который оборачивает broadcast ИМЕННО этого состояния — иначе кадры run'а не увидит ни одна открытая вкладка.

Соответствующие кадры: analysis_started (run_id, dialog_id, kind, messages_used, truncated), analysis_delta (run_id, dialog_id, text — уже коалесцированный ядром), analysis_done (run_id, dialog_id, ok, reason, input_tokens, output_tokens), analysis_error (run_id, dialog_id, message — текст от раннера ЦЕЛИКОМ, включая хвост stderr Claude CLI).
