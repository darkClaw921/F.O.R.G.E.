-- forge-meet, схема встреч.
--
-- Почему SQLite, а не JSON-стор по образцу PromptStore: час встречи это
-- 2000–4000 сегментов. Каждое сохранение переписывало бы файл целиком, а краш
-- посреди записи оставлял бы «последнюю удачную полную версию», то есть терял
-- бы кусок встречи. Здесь же строка сегмента доезжает на диск сама по себе.
--
-- Все времена — unix-секунды (INTEGER), кроме внутривстречных смещений: те в
-- миллисекундах от T0 = старта первой записи. Ось встречи ОДНА и строится
-- через performance.now() браузера; время бэкенда в неё не входит никогда,
-- иначе джиттер CDP просочился бы в таймлайн.

-- ---------------------------------------------------------------------------
-- Встречи
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS meetings (
    -- UUID v4. Он же имя каталога на диске: <cache_dir>/<id>/.
    id               TEXT    PRIMARY KEY,

    -- Идентичность комнаты (providers::MeetingLink::url_key). Query-параметров
    -- НЕ содержит: пароль Zoom и контекст Teams нужны для входа, но одну и ту
    -- же встречу рассылают и с ними, и без них.
    url_key          TEXT    NOT NULL,
    -- Куда бот реально идёт: параметры входа сохранены, трекинг срезан.
    canonical_url    TEXT    NOT NULL,
    platform         TEXT    NOT NULL,
    room_id          TEXT    NOT NULL,
    title            TEXT,

    -- Статус из crate::status::MeetStatus. Строковый, а не числовой: журнал
    -- событий и WS-кадры читают люди, а миграция числовых кодов при вставке
    -- нового этапа переписывала бы всю таблицу.
    status           TEXT    NOT NULL,
    -- Машиночитаемая причина последнего перехода (`whisper_unavailable`,
    -- `alone_timeout`, `user_cancelled`).
    status_reason    TEXT,
    -- СВОЙ статус саммари (crate::status::SummaryStatus). Отдельная колонка —
    -- это и есть механизм правила «failed только когда нет транскрипта»:
    -- провалу Claude есть куда записаться, не трогая статус встречи.
    summary_status   TEXT    NOT NULL DEFAULT 'none',

    -- normal | assisted. Ассистированный вход — человек проходит логин, капчу
    -- или лобби сам, бот при этом уже стоит с инжектом.
    mode             TEXT    NOT NULL DEFAULT 'normal',
    bot_name         TEXT,

    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    -- Момент старта ЗАПИСИ (не создания строки): от него фронт считает таймер.
    started_at       INTEGER,
    ended_at         INTEGER,
    duration_ms      INTEGER,

    -- Пути на диске. Хранятся, а не выводятся из id, потому что этапы
    -- идемпотентны именно по детерминированным путям: повторный прогон после
    -- перезапуска обязан увидеть тот же файл, а не создать второй.
    meeting_dir      TEXT    NOT NULL,
    audio_path       TEXT,
    transcript_path  TEXT,
    summary_path     TEXT,

    -- per_participant | slots | mixed — режим SFU, определённый в рантайме.
    -- Ф0 измерила Телемост: slots. Пока не измерена ни одна другая платформа,
    -- считать режим известным заранее нельзя ни для одной из них.
    track_mode       TEXT,
    -- Какой из трёх тапов дал звук: peer_connection | media_element | web_audio.
    capture_path     TEXT
);

-- Батч ленты и списки идут по статусу и времени.
CREATE INDEX IF NOT EXISTS ix_meetings_status     ON meetings(status);
CREATE INDEX IF NOT EXISTS ix_meetings_url_key    ON meetings(url_key);
CREATE INDEX IF NOT EXISTS ix_meetings_updated_at ON meetings(updated_at);

-- ОДНА АКТИВНАЯ ЗАПИСЬ НА КОМНАТУ — СТРУКТУРНО, А НЕ МЬЮТЕКСОМ.
--
-- Мьютекс в памяти защищает только один процесс и только пока тот жив; после
-- перезапуска devforge посреди записи он ничего не помнит. Частичный
-- уникальный индекс держит инвариант в самой базе, поэтому вторая попытка
-- старта на тот же url_key получает ошибку уникальности, а не вторую строку —
-- на этом стоит дедуп `attached: true`.
--
-- Список статусов ОБЯЗАН совпадать с crate::status::MeetStatus::ACTIVE; на
-- совпадение есть тест `active_statuses_match_the_partial_unique_index`.
CREATE UNIQUE INDEX IF NOT EXISTS ux_meetings_active_url ON meetings(url_key)
    WHERE status IN ('queued','launching','joining','waiting_admit','recording','leaving');

-- ---------------------------------------------------------------------------
-- Связь встречи с сообщением мессенджера
-- ---------------------------------------------------------------------------
-- Отдельная таблица, а не колонки в meetings: одну ссылку кидают в двух чатах
-- (и в двух мессенджерах), и обе кнопки обязаны показывать ОДНУ запись.
--
-- Инвариант идентификаторов проекта соблюдён: chat_id — СТРОКА (у Bitrix это
-- "chat7"/"sg12", у VK число, у Telegram число), message_id — ЧИСЛО.
--
-- WITHOUT ROWID: первичный ключ становится самой таблицей, поэтому батч ленты
-- (`plugin` + `chat_id` + диапазон `message_id`) — это один скан по префиксу
-- ключа, без похода в дополнительную структуру за строкой.
CREATE TABLE IF NOT EXISTS meeting_sources (
    plugin      TEXT    NOT NULL,
    chat_id     TEXT    NOT NULL,
    message_id  INTEGER NOT NULL,
    meeting_id  TEXT    NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (plugin, chat_id, message_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS ix_meeting_sources_meeting ON meeting_sources(meeting_id);

-- ---------------------------------------------------------------------------
-- Участники
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS meeting_participants (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id     TEXT    NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    -- Ключ внутри встречи: DOM-идентификатор плитки, msid трека или
    -- синтетический "track-3". Ключ, а не имя: имя может смениться по ходу.
    participant_key TEXT   NOT NULL,
    display_name   TEXT,
    -- Индекс трека. НЕ ПЕРЕИСПОЛЬЗУЕТСЯ после ended: иначе два человека
    -- склеились бы в один файл.
    track_idx      INTEGER,
    is_bot         INTEGER NOT NULL DEFAULT 0,
    -- dom | msid | ssrc | correlation | manual — каким шагом каскада получено
    -- имя. Нужно интерфейсу: «Участник 2» с ручным переименованием и имя из
    -- DOM выглядят по-разному.
    source         TEXT,
    confidence     REAL,
    first_seen_ms  INTEGER,
    last_seen_ms   INTEGER,
    UNIQUE (meeting_id, participant_key)
);

CREATE INDEX IF NOT EXISTS ix_participants_meeting ON meeting_participants(meeting_id);

-- ---------------------------------------------------------------------------
-- Интервалы речи
-- ---------------------------------------------------------------------------
-- Сюда пишут ОБА источника: пер-трековый VAD (режим per_participant) и
-- DOM-индикатор активного спикера (режимы slots и mixed, то есть в том числе
-- Телемост). Колонка source различает их, а attribute_segments работает с
-- любым из них — иначе платформа с одним общим треком осталась бы без
-- спикеров вовсе.
CREATE TABLE IF NOT EXISTS meeting_speaker_spans (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id     TEXT    NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    participant_id INTEGER REFERENCES meeting_participants(id) ON DELETE SET NULL,
    track_idx      INTEGER,
    start_ms       INTEGER NOT NULL,
    end_ms         INTEGER NOT NULL,
    source         TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS ix_spans_meeting_start ON meeting_speaker_spans(meeting_id, start_ms);

-- ---------------------------------------------------------------------------
-- Сегменты расшифровки
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS meeting_segments (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id     TEXT    NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    -- Порядковый номер внутри встречи. Уникален вместе с meeting_id: повторный
    -- прогон расшифровки заменяет сегменты, а не удваивает их.
    idx            INTEGER NOT NULL,
    start_ms       INTEGER NOT NULL,
    end_ms         INTEGER NOT NULL,
    text           TEXT    NOT NULL,
    participant_id INTEGER REFERENCES meeting_participants(id) ON DELETE SET NULL,
    speaker_name   TEXT,
    track_idx      INTEGER,
    -- dominant / Σoverlap. Ниже 0.6 при втором претенденте ≥25% сегмент
    -- режется по пословным меткам и помечается overlapped.
    confidence     REAL,
    overlapped     INTEGER NOT NULL DEFAULT 0,
    -- Потокенные метки whisper (-ojf) в JSON. Нужны, чтобы резать сегмент
    -- ровно на границе смены спикера: посегментные метки по 5–30 секунд
    -- регулярно её перекрывают.
    words_json     TEXT,
    UNIQUE (meeting_id, idx)
);

CREATE INDEX IF NOT EXISTS ix_segments_meeting_start ON meeting_segments(meeting_id, start_ms);

-- ---------------------------------------------------------------------------
-- Журнал переходов
-- ---------------------------------------------------------------------------
-- Единственный способ через сутки ответить, ПОЧЕМУ запись умерла. Строка
-- пишется в той же транзакции, что и UPDATE статуса, поэтому «статус сменился,
-- а записи нет» невозможно.
CREATE TABLE IF NOT EXISTS meeting_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id  TEXT    NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    at          INTEGER NOT NULL,
    -- NULL только у самой первой строки (создание встречи).
    from_status TEXT,
    to_status   TEXT    NOT NULL,
    reason      TEXT,
    detail      TEXT
);

CREATE INDEX IF NOT EXISTS ix_events_meeting ON meeting_events(meeting_id, id);
