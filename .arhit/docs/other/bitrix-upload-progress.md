# bitrix-upload-progress

Двухфазный прогресс отправки файла (Р8) — plugins/bitrix/src/progress.rs +
routes/upload.rs + ws/protocol.rs + static/js/bitrix/upload.js.

КАДР
  B24ServerMsg::UploadProgress { upload_id, phase, sent, total, indeterminate }
  phase: "client" | "portal" (PHASE_CLIENT / PHASE_PORTAL в progress.rs).
Адресация по upload_id обязательна: две параллельные отправки иначе путались бы
карточками.

ФАЗА client — браузер -> devforge
Считается по записанным байтам относительно Content-Length, поэтому проценты
ТОЧНЫЕ, indeterminate = false. Живёт внутри POST /api/bitrix/upload.

ФАЗА portal — devforge -> Bitrix24
Процентов НЕ СУЩЕСТВУЕТ ВООБЩЕ: im.v2.File.upload — один POST с base64 в теле,
и обратной связи о ходе портал не даёт. Кадр помечен indeterminate = true,
фронт рисует бегущую полосу. Врать процентами здесь запрещено: полоса, ползущая
по выдуманному закону, хуже честной бегущей — по ней принимают решение «ждать
или отменять».

UploadReporter двухфазный: new(...) стартует в client, begin_portal() переводит
во вторую фазу и СБРАСЫВАЕТ троттлер (смена фазы — отдельное событие
интерфейса «файл принят, отправляем в портал»; подавить его из-за того, что сто
миллисекунд назад ушёл кадр приёма, значило бы оставить полосу в состоянии
предыдущей фазы).

ФИНАЛЬНОЕ СОБЫТИЕ ПРОХОДИТ ВСЕГДА
Throttle::should_emit игнорирует оба условия подавления при is_final. В коде:
  stream_to_file зовёт finish_client(written) и на успехе, и на ЛЮБОЙ ошибке;
  send_staged_file зовёт finish(size) до возврата в обеих ветках.
Иначе полоса навсегда осталась бы на 97 % — визуально «загрузка зависла», хотя
файл давно на месте либо давно отвергнут. Это ЕДИНСТВЕННОЕ поведенческое
расхождение с панелью VK (там финал уходит только при успехе) и оно намеренное.
Признаком УСПЕХА кадр при этом не является: успех приезжает ответом REST, а
провал — ошибкой REST; строку фронт помечает по ответу, а не по прогрессу.

ТРОТТЛИНГ (общий с приёмом медиа Ф8)
MIN_INTERVAL 100 мс ЛИБО MIN_DELTA 256 КиБ. Медленную передачу двигает время,
быструю сдерживают байты. Без ограничения файл на сотню мегабайт дал бы десятки
тысяч кадров в broadcast ёмкостью BROADCAST_CAPACITY: все подписчики получили
бы Lagged и ушли бы в resync (Р19) — то есть ИНДИКАТОР ПРОГРЕССА СЛОМАЛ БЫ
REALTIME. Первый кадр проходит всегда: UI обязан увидеть «началось», не
дожидаясь ни ста миллисекунд, ни четверти мегабайта.

ProgressReader — байт-прозрачная обёртка над AsyncRead, считающая ИТОГ (а не
дельту: у дельты теряется последний кусок, если колбэк зовут не на каждом
чтении). Используется при сборке base64 из staging-файла через SharedReporter =
Arc<std::sync::Mutex<UploadReporter>>: колбэк дёргается из poll_read, то есть из
синхронного контекста, где await невозможен. Отравленный mutex молча
игнорируется — прогрессбар не причина ронять идущую отправку.

ФРОНТ (static/js/bitrix/upload.js)
Кадры фазы client приходят, но ЭТОЙ вкладкой не применяются: свой прогресс
приёма она знает из xhr.upload.onprogress точнее и без сокета, а upload_id
узнаёт только из ОТВЕТА — когда фаза уже кончилась. Кадры нужны ВТОРОЙ открытой
вкладке devforge, чтобы та видела занятую панель. Применяется только phase ===
'portal'.
Подпись этапа собирает format.js::uploadStageLabel; для 'portal' она не
содержит НИ ОДНОЙ ЦИФРЫ — это проверяется фронт-тестом регуляркой.

ТЕСТЫ
  progress.rs: the_upload_reporter_switches_to_an_indeterminate_portal_phase,
  the_final_frame_bypasses_the_throttle, progress_reader_is_byte_transparent;
  routes/upload.rs: the_client_phase_reports_growing_exact_progress,
  the_final_client_frame_arrives_even_on_failure;
  tests/upload.rs: the_portal_phase_is_indeterminate_and_always_finishes
  (финал приходит при ОШИБКЕ портала);
  frontend/bitrix_upload.test.mjs: подпись фазы portal без цифр, класс
  b24-upload-indeterminate на полосе, кадр чужого upload_id игнорируется.
