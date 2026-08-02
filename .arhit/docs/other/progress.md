# progress

plugins/telegram/src/progress.rs (крейт forge-telegram) — общий механизм отчётов о прогрессе передачи файлов для ОБОИХ направлений: приём медиа (download_progress) и отправка (upload_progress).

ЗАЧЕМ ОТДЕЛЬНЫЙ МОДУЛЬ: throttle обязан быть один на оба направления, иначе прогрессбар в UI ведёт себя по-разному на приёме и отправке. Без ограничения частоты 100-мегабайтный файл дал бы десятки тысяч кадров в broadcast ёмкостью 1024 -> все подписчики получают Lagged и уходят в resync, то есть индикатор прогресса СЛОМАЛ бы realtime.

Throttle: поля last_emit/last_bytes/min_interval(MIN_INTERVAL=100мс)/min_delta(MIN_DELTA=256 КиБ)/started. should_emit(current, is_final) -> bool: is_final ВСЕГДА true (иначе последнее событие может не дойти и прогрессбар навсегда замрёт на 97%); первое событие тоже всегда (UI обязан увидеть старт без ожидания); далее ЛЮБОЕ из условий — прошло >= min_interval ИЛИ прибавилось >= min_delta (медленную загрузку двигает время, быструю ограничивают байты). Метод мутирующий: true = 'событие считается отправленным', вызывать один раз на решение. Убывающий счётчик безопасен (saturating_sub).

ProgressReader<R, F>: обёртка над AsyncRead, считает прочитанные байты и дёргает FnMut(u64) с накопленным итогом. poll_read делегирует вложенному ридеру и прибавляет разницу buf.filled().len(); при n == 0 колбэк не зовётся. Прозрачность байт-в-байт — контракт (тест). Реализация для R: AsyncRead + Unpin, F: FnMut(u64) + Unpin — благодаря Unpin обходимся Pin::new(&mut inner) без pin-project. Нужен отправке медиа: upload_stream у grammers сам решает, когда читать, и ридер — единственная точка наблюдения.

DownloadReporter (state, dialog_id: String, message_id: i32, kind, throttle): report(received, total: Option<u64>) / finish(...) -> broadcast TgServerMsg::DownloadProgress. Свой throttle на КАЖДЫЙ медиа, поэтому параллельные загрузки не глушат друг друга. total: None означает 'размер неизвестен' -> фронт рисует индетерминированный индикатор.
UploadReporter (state, upload_id, total, throttle): report(sent)/finish(sent) -> TgServerMsg::UploadProgress.

СВЯЗИ: state.rs (broadcast_event), ws/protocol.rs (DownloadProgress/UploadProgress), routes/media.rs (DownloadReporter на каждый чанк iter_download). Отсутствие подписчиков — норма, SendError глотается в broadcast_event. 11 unit-тестов: throttle по времени/байтам/финалу, 10000 мелких приращений дают единицы событий, прозрачность ридера, кадры broadcast.
