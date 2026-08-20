# buildMeetSettings

tmux-web/static/js/settings/meet-tab.js — вкладка «Встречи» в модалке настроек. Ф16 добавила облачную половину.

ЧТО РИСУЕТ ПО ПОРЯДКУ: бейдж вычислителя и внешние бинари (Окружение) -> локальные модели whisper -> поля конфига по разделам MEET_GROUPS (bot, transcribe, summary, cloud, timeouts, storage, limits, vad) -> постоянный профиль браузера -> пути только для чтения -> Сохранить.

СЕТЬ ТОЛЬКО ИНЪЕКЦИЕЙ (deps): getMeetConfig, getMeetStatus, getModels, getCloudModels, getCloudBalance, putMeetConfig, downloadModel, recheckDeps, resetMeetProfile. Своих fetch у вкладки нет — на это есть проверка в тесте.

Ф16, ЧТО ДОБАВИЛОСЬ
1. Два НЕЗАВИСИМЫХ переключателя стадий — stt_backend и summary_backend, оба по умолчанию cloud. Приходят из EDITABLE как kind=choice со значениями cloud|local, подписываются BACKEND_LABELS. Под каждым живая подсказка backendNote: облако без ключа и локальная расшифровка без Metal не запустятся, и сказано об этом ДО сохранения, а не после встречи.
2. Ключ API (kind=secret) — текстовое поле с МАСКОЙ. Полный ключ во фронт не приходит никогда. collect() не отправляет значение, содержащее KEY_MASK_CHAR: бэкенд узнаёт свою маску и отвечает 400, а без этой проверки ключ затёрся бы строкой из точек.
3. Каталог облачных моделей (getCloudModels) грузится в общем Promise.all, но с .catch(() => null): молчащий polza.ai не имеет права оставить человека без формы, в которой чинится ключ. При наличии каталога поля cloud_stt_model и cloud_summary_model превращаются из ввода в список с ценами; модель из конфига, которой в каталоге нет, остаётся в списке отдельным пунктом, иначе открытие вкладки молча переключило бы стадию.
4. Остаток средств (getCloudBalance) — ОТДЕЛЬНЫМ запросом ПОСЛЕ отрисовки, промисом refs.balancePromise с catch. В GET /api/meet/status баланса намеренно нет: статус гейтит панель плагина.

СОХРАНЕНИЕ: collect() отправляет только ИЗМЕНЁННЫЕ поля (иначе форма затирала бы чужие правки файла), пропускает env_locked и маску секрета; ответ несёт saved/env_locked/fixes, и вкладка прямо пишет, что новые значения применятся на следующем запуске devforge.

Разметка строится узлами, без innerHTML. Хуки для тестов: data-meet-field, data-meet-key, data-meet-act, data-meet-backend, data-meet-backend-note, data-meet-model-note, data-meet-tone.
