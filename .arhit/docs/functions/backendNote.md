# backendNote

settings/meet-tab.js (Ф16): чистая функция подсказки под переключателем стадии. Вход: ключ поля (stt_backend|summary_backend), выбранное значение (cloud|local) и контекст из GET /api/meet/status ({cloud, deps}). Выход: {tone: ok|warn, text}.

ПРАВИЛА
- cloud + ключа нет (cloud.configured !== true) -> warn 'стадия не запустится, впишите ключ';
- cloud + cloud.error -> warn с текстом отказа сервиса;
- cloud + key.limit_remaining <= 0 -> warn 'лимит ключа исчерпан';
- cloud + всё в порядке -> ok, названа выбранная модель и сказано, что стадия стоит денег;
- local + stt + deps.gpu_metal !== true -> warn: правило проекта разрешает локальные модели ТОЛЬКО на GPU (Metal), пайплайн убивает прогон на CPU;
- local + stt + whisper не найден -> warn с подсказкой brew install whisper-cpp;
- local + stt в порядке -> ok, сказано про пословные метки времени, которых в облаке нет;
- local + summary -> ok, саммари считает локальный Claude CLI, транскрипт никуда не уходит.

Функция вызывается при построении поля и на каждом change селекта — расхождение видно немедленно, а не после сохранения.
