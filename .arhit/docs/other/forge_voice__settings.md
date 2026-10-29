# forge_voice::settings

Модуль plugins/voice/src/settings.rs — движок расшифровки, каким его правят из браузера. Ф4.

Настройка ОДНА на все мессенджеры и живёт в voice.toml, а не в user_settings.json: разбирать голосовые VK локально, а телеграмные в облаке никто не просил, зато 'почему в одной вкладке работает, а в другой нет' спросил бы каждый; плюс рядом в том же файле лежит ключ polza.ai, а секретам в user_settings.json не место (его фронт выгребает целиком).

API:
- Warning { code, message } и VoiceSettings { engine, cloud_key_set, warning }.
- parse_engine(raw) -> Option<Engine> — тот же словарь, что в voice.toml: local|whisper, cloud|polza.
- describe(cfg) -> VoiceSettings — гоняет gate::check с заведомо голосовым видом, чтобы узнать, чего не хватает выбранному движку.
- set_engine(voice_path, meet_path, engine) -> Result<VoiceSettings> — запись через config::write_fields (toml_edit + forge-atomic: комментарии, порядок ключей и чужие настройки целы; битый файл НЕ переписывается вслепую), затем конфиг ПЕРЕЧИТЫВАЕТСЯ с диска.

ДВА ПРАВИЛА. Ключ polza.ai наружу не отдаётся никогда — только признак cloud_key_set. Нерабочий выбор (local без весов, cloud без ключа) СОХРАНЯЕТСЯ и возвращает warning: отказ означал бы порядок 'сначала скачай полтора гигабайта вслепую, потом переключай', а молчаливое сохранение прежнего движка отправило бы следующее голосовое в облако вопреки только что сделанному выбору.

Потребители: роуты GET/PUT /api/{vk,telegram,bitrix}/voice/settings (plugins/*/src/routes/voice.rs) и фронт tmux-web/static/js/settings/voice-tab.js.
