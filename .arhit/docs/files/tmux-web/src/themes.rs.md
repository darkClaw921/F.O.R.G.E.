# tmux-web/src/themes.rs

Модуль модели тем и хранилища для tmux-web (Phase wk7).

## Назначение

Описывает цветовую тему UI + терминала + предоставляет атомарное файловое хранилище состояния тем (themes.json в data_dir, обычно ~/.config/forge/). Используется бэкендом для отдачи REST /api/themes* и фронтендом для применения CSS-переменных + xterm ITheme. Темы делятся на встроенные пресеты (компилируются в бинарь) и пользовательские (хранятся в themes.json).

## Структуры

### Theme
Полная цветовая тема. Поля:
- id: String — kebab-case slug, уникален во всём пространстве (presets + custom). Для пресетов фиксированный (default, dracula, ...); для custom — генерируется через uuid v4 в main.rs::create_custom_theme.
- name: String — отображаемое имя (например, 'Solarized Dark').
- ui: UiColors — палитра CSS-переменных.
- term: TermColors — палитра терминала.
Сериализация: serde camelCase. Все цвета — String '#RRGGBB'.

### UiColors (11 hex-значений)
- bg — основной фон body/панелей.
- bg_elev (camelCase: bgElev) — приподнятый фон (карточки, modal, header).
- fg — основной текст.
- fg_dim (fgDim) — приглушённый/вторичный текст.
- border — линии разделителей и рамки.
- accent — акцент (primary buttons, активная вкладка, focus ring).
- warn — внимание (жёлто-оранжевый, attention-флаг сессии).
- danger — ошибки и удаление.
- p0/p1/p2 — цвета priority-плашек задач (P0=критично, P1=high, P2=normal).

### TermColors (20 hex-значений → xterm.js ITheme)
- foreground/background — основные цвета терминала.
- cursor — цвет курсора.
- selection (camelCase) — фон выделения; на фронте маппится в selectionBackground.
- ANSI 8 базовых: black, red, green, yellow, blue, magenta, cyan, white.
- ANSI 8 ярких: bright_black .. bright_white (camelCase: brightBlack..brightWhite).

### ThemesState (envelope для themes.json)
- active: String — id активной темы. Может ссылаться на preset или custom; если ссылка битая, GET /api/themes/active делает fallback на 'default' с tracing::warn!.
- custom: Vec<Theme> — пользовательские темы. Пресеты в файл НЕ записываются. По #[serde(default)] поле опционально для миграционной совместимости.
- Default::default() = { active: 'default', custom: vec![] }.

## Функции

### built_in_presets() -> Vec<Theme>
Возвращает 9 пресетов в фиксированном порядке (id):
1. 'default' — Default (baseline tmux-web; 1:1 с :root в style.css, #0e1116/#d8dee9).
2. 'dracula' — Dracula (#282a36/#f8f8f2, accent #bd93f9).
3. 'solarized-dark' — Solarized Dark (#002b36/#839496, accent #268bd2).
4. 'solarized-light' — Solarized Light (#fdf6e3/#657b83, инверсия Solarized).
5. 'monokai' — Monokai (#272822/#f8f8f2, accent #a6e22e).
6. 'nord' — Nord (#2e3440/#d8dee9, accent #88c0d0).
7. 'gruvbox-dark' — Gruvbox Dark (#282828/#ebdbb2, accent #fabd2f).
8. 'one-dark' — One Dark (#282c34/#abb2bf, accent #61afef).
9. 'tokyo-night' — Tokyo Night (#1a1b26/#a9b1d6, accent #7aa2f7).

Каждый пресет полностью заполняет UiColors (11) и TermColors (20). Default используется как fallback при повреждённом state.active.

### find_preset(id: &str) -> Option<Theme>
Возвращает копию пресета по id (linear scan по built_in_presets).

### themes_file_path(data_dir: &Path) -> PathBuf
Хелпер: data_dir.join('themes.json').

### load(data_dir: &Path) -> ThemesState
Читает themes.json. Поведение fallback:
- файла нет → ThemesState::default() (без записи на диск).
- IO error → tracing::warn! + default.
- JSON parse error → tracing::warn! + default.
Гарантирует, что сервер всегда стартует, даже при битом файле.

### save(data_dir: &Path, state: &ThemesState) -> std::io::Result<()>
Атомарная запись. Алгоритм:
1. fs::create_dir_all(data_dir).
2. serde_json::to_vec_pretty(state).
3. Запись во временный <file>.tmp.
4. fs::rename(<file>.tmp, themes.json) — атомарно на POSIX в пределах одного mount-point.
1:1 со стратегией ProjectStore::save (projects.rs).

## Связи и использование

- main.rs: модуль импортируется как mod themes; AppState содержит themes (Arc<RwLock<ThemesState>>) + themes_dir (PathBuf, обычно registry_path.parent() = ~/.config/forge/). На старте: themes::load(&themes_dir) → AppState. Все REST-handler'ы темизации (5 endpoints) пишут через themes::save(&state.themes_dir, &s) под write-lock.
- REST endpoints: GET /api/themes (presets+custom+active.id), GET /api/themes/active, PATCH /api/themes/active, POST /api/themes/custom, PUT /api/themes/custom/:id, DELETE /api/themes/custom/:id.
- Фронтенд (app.js): mapTermTheme() ожидает camelCase (brightBlack, fgDim) — соответствует #[serde(rename_all = camelCase)].
- style.css :root preset Default 1:1 с built_in_presets()[0] — это инвариант (тест presets_count_and_unique_ids косвенно его подтверждает).

## Бизнес-логика и ограничения

- active.id может ссылаться как на preset, так и на custom. Поиск: сначала find_preset(id), потом state.custom.iter().find. Если не нашли нигде — fallback на 'default' пресет (с warn-логом).
- Пресеты иммутабельны: PUT /api/themes/custom/:id с id пресета → 409. POST с id, совпадающим с пресетом → 409.
- Активную тему нельзя удалить: DELETE /api/themes/custom/:id где state.active == id → 409.
- При создании custom без id (или с пустым/whitespace id) — генерируется uuid v4. Конфликт по id (presets ИЛИ custom) → 409.
- Persist гарантия: каждая мутация (PATCH active, POST/PUT/DELETE custom) сначала меняет state в памяти, затем сразу themes::save. При IO-ошибке возвращается 500, но изменение в памяти НЕ откатывается (best-effort durability — приемлемо для конфигурации).
- Тесты модуля: presets_count_and_unique_ids (9 уникальных id, default = baseline), camel_case_serde (round-trip с проверкой имён bgElev/fgDim/brightBlack), load_missing_returns_default, save_then_load_roundtrip, corrupt_file_falls_back_to_default, find_preset_works.

## Файл

- Путь: tmux-web/src/themes.rs.
- Размер: ~712 строк (включая 9 пресетов + тесты).
