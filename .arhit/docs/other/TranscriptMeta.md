# TranscriptMeta

Шапка расшифровки: всё, что не зависит от сегментов. Уезжает и в transcript.json (полем-flatten), и в шапку transcript.md.

ПОЛЯ: meeting_id, platform, url, title, started_at_unix_ms (дата ВСТРЕЧИ — свойство данных, а не прогона), duration_ms, engine, model, language, drift_scale.

ПОЛЕ engine (добавлено в Ф17). Чем расшифровано: константа ENGINE_LOCAL = 'whisper.cpp' у локального пути, ENGINE_CLOUD = 'polza.ai' у облачного. Существует потому, что model само по себе не отвечает на вопрос «кто это сделал»: и у облака, и у локального движка модель — просто строка.

КАКОЙ ДЕФЕКТ ЭТИМ ЗАКРЫТ. До Ф17 pipeline/run.rs заполнял model из cfg.whisper_model и language из cfg.whisper_language БЕЗУСЛОВНО, а render.rs печатал в markdown литерал 'Расшифровка: whisper.cpp'. Облачный прогон в итоге подписывался именем локальной модели: живая приёмка Ф17 получила transcript.md с «whisper.cpp, модель large-v3-turbo-q5_0» на записи, которую на самом деле расшифровало облако openai/whisper-1 (подтверждено списанием 5,50 ₽ с баланса polza.ai и manifest.postprocess.per_track=0). По артефакту нельзя было отличить один путь расшифровки от другого — ровно то, что требует проверять приёмка обоих путей.

КАК ЗАПОЛНЯЕТСЯ. pipeline/run.rs выбирает по флагу cloud_stt: engine/model/language берутся из cfg.cloud() (stt_model, stt_language) на облачном пути и из cfg.whisper_model/whisper_language на локальном.

СОВМЕСТИМОСТЬ. engine помечен serde(default = default_engine) и восстанавливается как 'whisper.cpp' у записей, сделанных до появления поля: локальный путь был единственным на момент их создания, поэтому старое значение восстанавливается однозначно, а не угадывается.

ВРЕМЕНИ ГЕНЕРАЦИИ В ФАЙЛАХ НЕТ намеренно: повтор этапа (retry?from=transcribing) обязан быть отличим от «ничего не изменилось», а с меткой времени каждый прогон выглядел бы новым результатом.
