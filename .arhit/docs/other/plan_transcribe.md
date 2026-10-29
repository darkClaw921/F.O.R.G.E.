# plan_transcribe

plugins/meet/src/deps.rs, метод DepsReport: выбор пути расшифровки по таблице choose_route. Локальная сторона рабочая при whisper + модель + Metal (transcribe_blocker), облачная — при ключе и ffmpeg (cloud_transcribe_blocker; до первой пробы ffmpeg не мешает). Возвращает StagePlan либо код отказа заказанного пути. transcribe_blocker_for — тонкая обёртка .err() для лестницы 409. Зовут: pipeline::run (развилка стадии, cloud_stt), routes::meetings::start_stage, routes::status.
