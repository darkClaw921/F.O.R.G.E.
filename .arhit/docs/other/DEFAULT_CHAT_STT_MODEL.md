# DEFAULT_CHAT_STT_MODEL

Модель облачного STT для голосовых в чатах, plugins/voice/src/cloud/mod.rs. Значение: openai/whisper-large-v3-turbo, 0,048 руб/мин по живому каталогу polza.ai (GET /models, top_provider.pricing.stt_per_minute, август 2026).

ЗАЧЕМ ОТДЕЛЬНАЯ КОНСТАНТА. У встреч дефолт другой — DEFAULT_STT_MODEL = openai/whisper-1, 0,432 руб/мин. Платят там ровно за таймкоды: атрибуция спикеров построена на пересечении интервалов, без них расшифровку встречи не на что положить. У голосового в чате под волной показывается ТЕКСТ, таймлайна нет вовсе, поэтому платить вдевятеро за неотображаемые сегменты — чистый убыток.

АЛЬТЕРНАТИВЫ ИЗ КАТАЛОГА (руб/мин): openai/whisper-1 0,432 (с таймкодами); openai/gpt-4o-mini-transcribe 0,344; mistralai/voxtral-mini-transcribe 0,216; qwen/qwen3-asr-flash 0,151; aiesa/transcribe 0,120; nvidia/parakeet-tdt-0.6b-v3 0,108; openai/whisper-large-v3 0,108; openai/whisper-large-v3-turbo 0,048. Выбран самый дешёвый, и он же совпадает с моделью локального движка (whisper::DEFAULT_WHISPER_MODEL = large-v3-turbo-q5_0) — облако и GPU дают на чатах сопоставимый результат.

ОБЯЗАТЕЛЬНЫЙ НЮАНС. Модель идёт через OpenRouter Audio API, который на response_format=verbose_json отвечает BAD_REQUEST ('Допустимые значения: json, text'), то есть теряет расшифровку целиком, а не деградирует. Поэтому формат ответа выводится из модели функцией stt_response_format, а не задан полем конфига: поле разъехалось бы с моделью на первой же смене дефолта. Ответ вида {"text": "…"} разбирает та же parse_verbose_json (ветка 'сегментов нет, но текст есть').

Живая проба на русской фразе через настоящий API дала дословный текст. Инварианты закреплены тестами the_chat_default_is_the_cheap_model_and_not_the_meeting_one и the_response_format_follows_the_model.
