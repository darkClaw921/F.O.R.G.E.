//! Theme model and storage for tmux-web (Phase wk7).
//!
//! ### Назначение
//!
//! Описывает цветовую тему UI + терминала: `Theme` содержит `UiColors`
//! (11 переменных для CSS — фон, текст, акценты, границы и т.д.) и
//! `TermColors` (20 цветов терминала: 16 ANSI + foreground/background/
//! cursor/selection). Темы делятся на встроенные пресеты (`built_in_presets`)
//! и пользовательские (`ThemesState::custom`), хранятся в `themes.json`
//! рядом с `projects.json` (`~/.config/forge/`).
//!
//! ### Сериализация
//!
//! Все структуры сериализуются в JSON через `serde`. Поля переименовываются
//! в camelCase (`#[serde(rename_all = "camelCase")]`) — это нужно для
//! фронтенда (`bgElev`, `brightBlack`, и т.д.). Все цвета хранятся как
//! `String` в формате `#RRGGBB` (lowercase hex с префиксом `#`). Опциональных
//! полей нет — каждая тема обязана задать все 11 + 20 цветов.
//!
//! ### Файловое хранилище
//!
//! `ThemesState { active, custom }` сериализуется в `themes.json`. Сохранение
//! атомарное: пишем во временный `themes.json.tmp`, затем `rename` поверх.
//! Тот же подход, что и в `projects.rs`, чтобы не плодить зависимости и
//! поведение совпадало с остальной конфигурацией.
//!
//! ### REST API (см. `main.rs`)
//!
//! - `GET /api/themes` — `{ presets, custom, active }`.
//! - `GET /api/themes/active` — полный `Theme` активной темы.
//! - `PATCH /api/themes/active` — переключить активную.
//! - `POST /api/themes/custom` — добавить пользовательскую тему.
//! - `PUT /api/themes/custom/:id` — заменить пользовательскую.
//! - `DELETE /api/themes/custom/:id` — удалить (запрет если активна).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// =============================================================================
// Модель темы
// =============================================================================

/// Полная цветовая тема: id, человекочитаемое имя, UI-цвета, цвета терминала.
///
/// `id` — kebab-case slug, уникальный во всём пространстве (presets + custom).
/// Для пресетов id фиксированный (`default`, `dracula`, …); для custom
/// генерируется через `uuid::Uuid::new_v4()` (см. `main.rs` POST handler).
///
/// `name` — отображается во фронтенде в карточке темы. Для пресетов —
/// каноничные названия (например, `Solarized Dark`).
///
/// `ui` и `term` — обе обязательные секции, см. их doc-comment'ы.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub ui: UiColors,
    pub term: TermColors,
}

/// Цвета UI (CSS-переменные) — 11 значений, каждое hex `#RRGGBB`.
///
/// Семантика полей:
/// - `bg` — основной фон (body / большие панели).
/// - `bg_elev` — приподнятый фон (карточки, modal'ы, header'ы).
/// - `fg` — основной цвет текста.
/// - `fg_dim` — приглушённый текст (вторичная информация, hint'ы).
/// - `border` — цвет линий-разделителей и рамок.
/// - `accent` — акцентный цвет (primary buttons, активная вкладка, focus ring).
/// - `warn` — жёлто-оранжевый (внимание, attention-флаг сессии).
/// - `danger` — красный (ошибки, удаление).
/// - `p0`/`p1`/`p2` — цвета для priority-плашек задач (P0=критично, …).
///
/// JSON-имена в camelCase: `bg`, `bgElev`, `fg`, `fgDim`, `border`, `accent`,
/// `warn`, `danger`, `p0`, `p1`, `p2`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UiColors {
    pub bg: String,
    pub bg_elev: String,
    pub fg: String,
    pub fg_dim: String,
    pub border: String,
    pub accent: String,
    pub warn: String,
    pub danger: String,
    pub p0: String,
    pub p1: String,
    pub p2: String,
}

/// Цвета терминала — 20 значений, маппятся на xterm.js `ITheme`.
///
/// `foreground`/`background` — основные; `cursor` — цвет курсора;
/// `selection` — фон выделения. Остальные 16 — стандартная ANSI-палитра:
/// 8 базовых (`black`..`white`) + 8 ярких (`bright_black`..`bright_white`).
///
/// JSON-имена в camelCase: `foreground`, `background`, `cursor`, `selection`,
/// `black`, …, `white`, `brightBlack`, `brightRed`, …, `brightWhite`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TermColors {
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub selection: String,
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
}

// =============================================================================
// File-state: themes.json
// =============================================================================

/// Состояние хранилища тем (envelope для `themes.json`).
///
/// - `active` — id активной темы (может ссылаться как на пресет, так и на
///   custom). При повреждении (id не найден ни там, ни там) caller'ы должны
///   делать fallback на `default` пресет (см. `GET /api/themes/active`).
/// - `custom` — список пользовательских тем. Пресеты НЕ сохраняются в файле
///   (они компилируются в бинарь через `built_in_presets`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemesState {
    pub active: String,
    #[serde(default)]
    pub custom: Vec<Theme>,
}

impl Default for ThemesState {
    fn default() -> Self {
        Self {
            active: "default".to_string(),
            custom: Vec::new(),
        }
    }
}

/// Возвращает путь к файлу `themes.json` внутри data-каталога.
///
/// `data_dir` обычно `~/.config/forge/` — то же место, что и `projects.json`.
pub fn themes_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join("themes.json")
}

/// Загружает состояние тем с диска.
///
/// При отсутствии файла или ошибке парсинга — возвращает `ThemesState::default()`
/// (`active = "default"`, `custom = []`). Не паникует и не падает: кривой
/// файл логируется через `tracing::warn!` и заменяется дефолтом в памяти.
/// На диск дефолт НЕ записывается — это сделает первый `save()`.
pub fn load(data_dir: &Path) -> ThemesState {
    let path = themes_file_path(data_dir);
    if !path.exists() {
        return ThemesState::default();
    }
    let raw = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = ?e, "themes.json read failed; using default");
            return ThemesState::default();
        }
    };
    match serde_json::from_slice::<ThemesState>(&raw) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = ?e, "themes.json parse failed; using default");
            ThemesState::default()
        }
    }
}

/// Атомарно сохраняет состояние тем в `themes.json`.
///
/// Стратегия (1:1 с `ProjectStore::save`): пишем в `<file>.tmp`, затем
/// `rename` поверх старого. На POSIX rename атомарен в пределах одного
/// mount-point. Каталог `data_dir` создаётся, если его нет.
pub fn save(data_dir: &Path, state: &ThemesState) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let body = serde_json::to_vec_pretty(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let path = themes_file_path(data_dir);

    let mut tmp = path.clone();
    let mut tmp_name = tmp
        .file_name()
        .map(|s| s.to_owned())
        .unwrap_or_default();
    tmp_name.push(".tmp");
    tmp.set_file_name(tmp_name);

    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

// =============================================================================
// Built-in presets — 13 тем
// =============================================================================

/// Хелпер для лаконичного построения `String` из строкового литерала.
fn s(v: &str) -> String {
    v.to_string()
}

/// Возвращает 13 встроенных пресетов в фиксированном порядке.
///
/// Список (в порядке возврата): Default, Dracula, Solarized Dark,
/// Solarized Light, Monokai, Nord, Gruvbox Dark, One Dark, Tokyo Night,
/// Catppuccin Latte, Catppuccin Frappé, Catppuccin Macchiato, Catppuccin Mocha.
///
/// Для каждой темы:
/// - `ui` — 11 цветов, подобранных под палитру (фон/текст/акцент/состояния).
/// - `term` — 20 цветов из официальных репозиториев соответствующих тем.
///
/// Default — baseline tmux-web (его палитра 1:1 с текущими hex-литералами в
/// `style.css` и xterm-конфигом в `app.js`), используется как fallback при
/// повреждённом state.active.
pub fn built_in_presets() -> Vec<Theme> {
    vec![
        // ---------------------------------------------------------------
        // 1. Default — текущий baseline tmux-web (#0e1116 / #d8dee9).
        // ---------------------------------------------------------------
        Theme {
            id: s("default"),
            name: s("Default"),
            ui: UiColors {
                bg: s("#0e1116"),
                bg_elev: s("#161b22"),
                fg: s("#d8dee9"),
                fg_dim: s("#8b949e"),
                border: s("#30363d"),
                accent: s("#2a7fff"),
                warn: s("#d29922"),
                danger: s("#f85149"),
                p0: s("#f85149"),
                p1: s("#d29922"),
                p2: s("#2a7fff"),
            },
            term: TermColors {
                foreground: s("#d8dee9"),
                background: s("#0e1116"),
                cursor: s("#d8dee9"),
                selection: s("#3b4252"),
                black: s("#3b4252"),
                red: s("#bf616a"),
                green: s("#a3be8c"),
                yellow: s("#ebcb8b"),
                blue: s("#81a1c1"),
                magenta: s("#b48ead"),
                cyan: s("#88c0d0"),
                white: s("#e5e9f0"),
                bright_black: s("#4c566a"),
                bright_red: s("#bf616a"),
                bright_green: s("#a3be8c"),
                bright_yellow: s("#ebcb8b"),
                bright_blue: s("#81a1c1"),
                bright_magenta: s("#b48ead"),
                bright_cyan: s("#8fbcbb"),
                bright_white: s("#eceff4"),
            },
        },
        // ---------------------------------------------------------------
        // 2. Dracula — официальная палитра dracula/dracula-theme.
        // ---------------------------------------------------------------
        Theme {
            id: s("dracula"),
            name: s("Dracula"),
            ui: UiColors {
                bg: s("#282a36"),
                bg_elev: s("#343746"),
                fg: s("#f8f8f2"),
                fg_dim: s("#6272a4"),
                border: s("#44475a"),
                accent: s("#bd93f9"),
                warn: s("#ffb86c"),
                danger: s("#ff5555"),
                p0: s("#ff5555"),
                p1: s("#ffb86c"),
                p2: s("#8be9fd"),
            },
            term: TermColors {
                foreground: s("#f8f8f2"),
                background: s("#282a36"),
                cursor: s("#f8f8f2"),
                selection: s("#44475a"),
                black: s("#21222c"),
                red: s("#ff5555"),
                green: s("#50fa7b"),
                yellow: s("#f1fa8c"),
                blue: s("#bd93f9"),
                magenta: s("#ff79c6"),
                cyan: s("#8be9fd"),
                white: s("#f8f8f2"),
                bright_black: s("#6272a4"),
                bright_red: s("#ff6e6e"),
                bright_green: s("#69ff94"),
                bright_yellow: s("#ffffa5"),
                bright_blue: s("#d6acff"),
                bright_magenta: s("#ff92df"),
                bright_cyan: s("#a4ffff"),
                bright_white: s("#ffffff"),
            },
        },
        // ---------------------------------------------------------------
        // 3. Solarized Dark — Ethan Schoonover's palette.
        // ---------------------------------------------------------------
        Theme {
            id: s("solarized-dark"),
            name: s("Solarized Dark"),
            ui: UiColors {
                bg: s("#002b36"),
                bg_elev: s("#073642"),
                fg: s("#839496"),
                fg_dim: s("#586e75"),
                border: s("#073642"),
                accent: s("#268bd2"),
                warn: s("#b58900"),
                danger: s("#dc322f"),
                p0: s("#dc322f"),
                p1: s("#b58900"),
                p2: s("#268bd2"),
            },
            term: TermColors {
                foreground: s("#839496"),
                background: s("#002b36"),
                cursor: s("#93a1a1"),
                selection: s("#073642"),
                black: s("#073642"),
                red: s("#dc322f"),
                green: s("#859900"),
                yellow: s("#b58900"),
                blue: s("#268bd2"),
                magenta: s("#d33682"),
                cyan: s("#2aa198"),
                white: s("#eee8d5"),
                bright_black: s("#002b36"),
                bright_red: s("#cb4b16"),
                bright_green: s("#586e75"),
                bright_yellow: s("#657b83"),
                bright_blue: s("#839496"),
                bright_magenta: s("#6c71c4"),
                bright_cyan: s("#93a1a1"),
                bright_white: s("#fdf6e3"),
            },
        },
        // ---------------------------------------------------------------
        // 4. Solarized Light — same palette, inverted base.
        // ---------------------------------------------------------------
        Theme {
            id: s("solarized-light"),
            name: s("Solarized Light"),
            ui: UiColors {
                bg: s("#fdf6e3"),
                bg_elev: s("#eee8d5"),
                fg: s("#657b83"),
                fg_dim: s("#93a1a1"),
                border: s("#eee8d5"),
                accent: s("#268bd2"),
                warn: s("#b58900"),
                danger: s("#dc322f"),
                p0: s("#dc322f"),
                p1: s("#b58900"),
                p2: s("#268bd2"),
            },
            term: TermColors {
                foreground: s("#657b83"),
                background: s("#fdf6e3"),
                cursor: s("#586e75"),
                selection: s("#eee8d5"),
                black: s("#073642"),
                red: s("#dc322f"),
                green: s("#859900"),
                yellow: s("#b58900"),
                blue: s("#268bd2"),
                magenta: s("#d33682"),
                cyan: s("#2aa198"),
                white: s("#eee8d5"),
                bright_black: s("#002b36"),
                bright_red: s("#cb4b16"),
                bright_green: s("#586e75"),
                bright_yellow: s("#657b83"),
                bright_blue: s("#839496"),
                bright_magenta: s("#6c71c4"),
                bright_cyan: s("#93a1a1"),
                bright_white: s("#fdf6e3"),
            },
        },
        // ---------------------------------------------------------------
        // 5. Monokai — classic Wimer Hazenberg palette.
        // ---------------------------------------------------------------
        Theme {
            id: s("monokai"),
            name: s("Monokai"),
            ui: UiColors {
                bg: s("#272822"),
                bg_elev: s("#3e3d32"),
                fg: s("#f8f8f2"),
                fg_dim: s("#75715e"),
                border: s("#49483e"),
                accent: s("#a6e22e"),
                warn: s("#fd971f"),
                danger: s("#f92672"),
                p0: s("#f92672"),
                p1: s("#fd971f"),
                p2: s("#66d9ef"),
            },
            term: TermColors {
                foreground: s("#f8f8f2"),
                background: s("#272822"),
                cursor: s("#f8f8f0"),
                selection: s("#49483e"),
                black: s("#272822"),
                red: s("#f92672"),
                green: s("#a6e22e"),
                yellow: s("#f4bf75"),
                blue: s("#66d9ef"),
                magenta: s("#ae81ff"),
                cyan: s("#a1efe4"),
                white: s("#f8f8f2"),
                bright_black: s("#75715e"),
                bright_red: s("#f92672"),
                bright_green: s("#a6e22e"),
                bright_yellow: s("#f4bf75"),
                bright_blue: s("#66d9ef"),
                bright_magenta: s("#ae81ff"),
                bright_cyan: s("#a1efe4"),
                bright_white: s("#f9f8f5"),
            },
        },
        // ---------------------------------------------------------------
        // 6. Nord — arcticicestudio/nord (polar night + snow storm).
        // ---------------------------------------------------------------
        Theme {
            id: s("nord"),
            name: s("Nord"),
            ui: UiColors {
                bg: s("#2e3440"),
                bg_elev: s("#3b4252"),
                fg: s("#d8dee9"),
                fg_dim: s("#7b8394"),
                border: s("#434c5e"),
                accent: s("#88c0d0"),
                warn: s("#ebcb8b"),
                danger: s("#bf616a"),
                p0: s("#bf616a"),
                p1: s("#ebcb8b"),
                p2: s("#81a1c1"),
            },
            term: TermColors {
                foreground: s("#d8dee9"),
                background: s("#2e3440"),
                cursor: s("#d8dee9"),
                selection: s("#434c5e"),
                black: s("#3b4252"),
                red: s("#bf616a"),
                green: s("#a3be8c"),
                yellow: s("#ebcb8b"),
                blue: s("#81a1c1"),
                magenta: s("#b48ead"),
                cyan: s("#88c0d0"),
                white: s("#e5e9f0"),
                bright_black: s("#4c566a"),
                bright_red: s("#bf616a"),
                bright_green: s("#a3be8c"),
                bright_yellow: s("#ebcb8b"),
                bright_blue: s("#81a1c1"),
                bright_magenta: s("#b48ead"),
                bright_cyan: s("#8fbcbb"),
                bright_white: s("#eceff4"),
            },
        },
        // ---------------------------------------------------------------
        // 7. Gruvbox Dark — morhetz/gruvbox (medium contrast).
        // ---------------------------------------------------------------
        Theme {
            id: s("gruvbox-dark"),
            name: s("Gruvbox Dark"),
            ui: UiColors {
                bg: s("#282828"),
                bg_elev: s("#3c3836"),
                fg: s("#ebdbb2"),
                fg_dim: s("#a89984"),
                border: s("#504945"),
                accent: s("#fabd2f"),
                warn: s("#fe8019"),
                danger: s("#fb4934"),
                p0: s("#fb4934"),
                p1: s("#fe8019"),
                p2: s("#83a598"),
            },
            term: TermColors {
                foreground: s("#ebdbb2"),
                background: s("#282828"),
                cursor: s("#ebdbb2"),
                selection: s("#504945"),
                black: s("#282828"),
                red: s("#cc241d"),
                green: s("#98971a"),
                yellow: s("#d79921"),
                blue: s("#458588"),
                magenta: s("#b16286"),
                cyan: s("#689d6a"),
                white: s("#a89984"),
                bright_black: s("#928374"),
                bright_red: s("#fb4934"),
                bright_green: s("#b8bb26"),
                bright_yellow: s("#fabd2f"),
                bright_blue: s("#83a598"),
                bright_magenta: s("#d3869b"),
                bright_cyan: s("#8ec07c"),
                bright_white: s("#ebdbb2"),
            },
        },
        // ---------------------------------------------------------------
        // 8. One Dark — atom/one-dark-syntax.
        // ---------------------------------------------------------------
        Theme {
            id: s("one-dark"),
            name: s("One Dark"),
            ui: UiColors {
                bg: s("#282c34"),
                bg_elev: s("#21252b"),
                fg: s("#abb2bf"),
                fg_dim: s("#5c6370"),
                border: s("#3e4451"),
                accent: s("#61afef"),
                warn: s("#e5c07b"),
                danger: s("#e06c75"),
                p0: s("#e06c75"),
                p1: s("#e5c07b"),
                p2: s("#61afef"),
            },
            term: TermColors {
                foreground: s("#abb2bf"),
                background: s("#282c34"),
                cursor: s("#528bff"),
                selection: s("#3e4451"),
                black: s("#282c34"),
                red: s("#e06c75"),
                green: s("#98c379"),
                yellow: s("#e5c07b"),
                blue: s("#61afef"),
                magenta: s("#c678dd"),
                cyan: s("#56b6c2"),
                white: s("#abb2bf"),
                bright_black: s("#5c6370"),
                bright_red: s("#e06c75"),
                bright_green: s("#98c379"),
                bright_yellow: s("#d19a66"),
                bright_blue: s("#61afef"),
                bright_magenta: s("#c678dd"),
                bright_cyan: s("#56b6c2"),
                bright_white: s("#ffffff"),
            },
        },
        // ---------------------------------------------------------------
        // 9. Tokyo Night — enkia/tokyo-night (storm/dark variant).
        // ---------------------------------------------------------------
        Theme {
            id: s("tokyo-night"),
            name: s("Tokyo Night"),
            ui: UiColors {
                bg: s("#1a1b26"),
                bg_elev: s("#24283b"),
                fg: s("#a9b1d6"),
                fg_dim: s("#565f89"),
                border: s("#2f334d"),
                accent: s("#7aa2f7"),
                warn: s("#e0af68"),
                danger: s("#f7768e"),
                p0: s("#f7768e"),
                p1: s("#e0af68"),
                p2: s("#7aa2f7"),
            },
            term: TermColors {
                foreground: s("#a9b1d6"),
                background: s("#1a1b26"),
                cursor: s("#c0caf5"),
                selection: s("#33467c"),
                black: s("#15161e"),
                red: s("#f7768e"),
                green: s("#9ece6a"),
                yellow: s("#e0af68"),
                blue: s("#7aa2f7"),
                magenta: s("#bb9af7"),
                cyan: s("#7dcfff"),
                white: s("#a9b1d6"),
                bright_black: s("#414868"),
                bright_red: s("#f7768e"),
                bright_green: s("#9ece6a"),
                bright_yellow: s("#e0af68"),
                bright_blue: s("#7aa2f7"),
                bright_magenta: s("#bb9af7"),
                bright_cyan: s("#7dcfff"),
                bright_white: s("#c0caf5"),
            },
        },
        // ---------------------------------------------------------------
        // 10. Catppuccin Latte — light flavor (catppuccin/catppuccin).
        // ---------------------------------------------------------------
        Theme {
            id: s("catppuccin-latte"),
            name: s("Catppuccin Latte"),
            ui: UiColors {
                bg: s("#eff1f5"),
                bg_elev: s("#e6e9ef"),
                fg: s("#4c4f69"),
                fg_dim: s("#6c6f85"),
                border: s("#ccd0da"),
                accent: s("#1e66f5"),
                warn: s("#df8e1d"),
                danger: s("#d20f39"),
                p0: s("#d20f39"),
                p1: s("#fe640b"),
                p2: s("#1e66f5"),
            },
            term: TermColors {
                foreground: s("#4c4f69"),
                background: s("#eff1f5"),
                cursor: s("#dc8a78"),
                selection: s("#acb0be"),
                black: s("#5c5f77"),
                red: s("#d20f39"),
                green: s("#40a02b"),
                yellow: s("#df8e1d"),
                blue: s("#1e66f5"),
                magenta: s("#ea76cb"),
                cyan: s("#179299"),
                white: s("#acb0be"),
                bright_black: s("#6c6f85"),
                bright_red: s("#d20f39"),
                bright_green: s("#40a02b"),
                bright_yellow: s("#df8e1d"),
                bright_blue: s("#1e66f5"),
                bright_magenta: s("#ea76cb"),
                bright_cyan: s("#179299"),
                bright_white: s("#bcc0cc"),
            },
        },
        // ---------------------------------------------------------------
        // 11. Catppuccin Frappé — medium-dark flavor.
        // ---------------------------------------------------------------
        Theme {
            id: s("catppuccin-frappe"),
            name: s("Catppuccin Frappé"),
            ui: UiColors {
                bg: s("#303446"),
                bg_elev: s("#292c3c"),
                fg: s("#c6d0f5"),
                fg_dim: s("#a5adce"),
                border: s("#414559"),
                accent: s("#8caaee"),
                warn: s("#e5c890"),
                danger: s("#e78284"),
                p0: s("#e78284"),
                p1: s("#ef9f76"),
                p2: s("#8caaee"),
            },
            term: TermColors {
                foreground: s("#c6d0f5"),
                background: s("#303446"),
                cursor: s("#f2d5cf"),
                selection: s("#414559"),
                black: s("#51576d"),
                red: s("#e78284"),
                green: s("#a6d189"),
                yellow: s("#e5c890"),
                blue: s("#8caaee"),
                magenta: s("#f4b8e4"),
                cyan: s("#81c8be"),
                white: s("#b5bfe2"),
                bright_black: s("#626880"),
                bright_red: s("#e78284"),
                bright_green: s("#a6d189"),
                bright_yellow: s("#e5c890"),
                bright_blue: s("#8caaee"),
                bright_magenta: s("#f4b8e4"),
                bright_cyan: s("#81c8be"),
                bright_white: s("#a5adce"),
            },
        },
        // ---------------------------------------------------------------
        // 12. Catppuccin Macchiato — darker flavor.
        // ---------------------------------------------------------------
        Theme {
            id: s("catppuccin-macchiato"),
            name: s("Catppuccin Macchiato"),
            ui: UiColors {
                bg: s("#24273a"),
                bg_elev: s("#1e2030"),
                fg: s("#cad3f5"),
                fg_dim: s("#a5adcb"),
                border: s("#363a4f"),
                accent: s("#8aadf4"),
                warn: s("#eed49f"),
                danger: s("#ed8796"),
                p0: s("#ed8796"),
                p1: s("#f5a97f"),
                p2: s("#8aadf4"),
            },
            term: TermColors {
                foreground: s("#cad3f5"),
                background: s("#24273a"),
                cursor: s("#f4dbd6"),
                selection: s("#363a4f"),
                black: s("#494d64"),
                red: s("#ed8796"),
                green: s("#a6da95"),
                yellow: s("#eed49f"),
                blue: s("#8aadf4"),
                magenta: s("#f5bde6"),
                cyan: s("#8bd5ca"),
                white: s("#b8c0e0"),
                bright_black: s("#5b6078"),
                bright_red: s("#ed8796"),
                bright_green: s("#a6da95"),
                bright_yellow: s("#eed49f"),
                bright_blue: s("#8aadf4"),
                bright_magenta: s("#f5bde6"),
                bright_cyan: s("#8bd5ca"),
                bright_white: s("#a5adcb"),
            },
        },
        // ---------------------------------------------------------------
        // 13. Catppuccin Mocha — darkest flavor (default Catppuccin).
        // ---------------------------------------------------------------
        Theme {
            id: s("catppuccin-mocha"),
            name: s("Catppuccin Mocha"),
            ui: UiColors {
                bg: s("#1e1e2e"),
                bg_elev: s("#181825"),
                fg: s("#cdd6f4"),
                fg_dim: s("#a6adc8"),
                border: s("#313244"),
                accent: s("#89b4fa"),
                warn: s("#f9e2af"),
                danger: s("#f38ba8"),
                p0: s("#f38ba8"),
                p1: s("#fab387"),
                p2: s("#89b4fa"),
            },
            term: TermColors {
                foreground: s("#cdd6f4"),
                background: s("#1e1e2e"),
                cursor: s("#f5e0dc"),
                selection: s("#313244"),
                black: s("#45475a"),
                red: s("#f38ba8"),
                green: s("#a6e3a1"),
                yellow: s("#f9e2af"),
                blue: s("#89b4fa"),
                magenta: s("#f5c2e7"),
                cyan: s("#94e2d5"),
                white: s("#bac2de"),
                bright_black: s("#585b70"),
                bright_red: s("#f38ba8"),
                bright_green: s("#a6e3a1"),
                bright_yellow: s("#f9e2af"),
                bright_blue: s("#89b4fa"),
                bright_magenta: s("#f5c2e7"),
                bright_cyan: s("#94e2d5"),
                bright_white: s("#a6adc8"),
            },
        },
    ]
}

/// Возвращает пресет по id, если такой есть.
pub fn find_preset(id: &str) -> Option<Theme> {
    built_in_presets().into_iter().find(|t| t.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("forge-themes-{tag}-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn presets_count_and_unique_ids() {
        let presets = built_in_presets();
        assert_eq!(presets.len(), 13);
        let mut ids: Vec<&str> = presets.iter().map(|t| t.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 13, "preset ids must be unique");
        // Default присутствует.
        assert!(presets.iter().any(|t| t.id == "default"));
        // Default = baseline tmux-web.
        let def = presets.iter().find(|t| t.id == "default").unwrap();
        assert_eq!(def.ui.bg, "#0e1116");
        assert_eq!(def.ui.fg, "#d8dee9");
    }

    #[test]
    fn camel_case_serde() {
        let p = built_in_presets().into_iter().next().unwrap();
        let json = serde_json::to_string(&p).unwrap();
        // camelCase для составных полей.
        assert!(json.contains("\"bgElev\""));
        assert!(json.contains("\"fgDim\""));
        assert!(json.contains("\"brightBlack\""));
        assert!(json.contains("\"brightWhite\""));
        // Round-trip.
        let back: Theme = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn load_missing_returns_default() {
        let dir = tempdir("missing");
        let s = load(&dir);
        assert_eq!(s.active, "default");
        assert!(s.custom.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempdir("rt");
        let mut state = ThemesState::default();
        state.active = "dracula".to_string();
        let custom = Theme {
            id: "my-custom".to_string(),
            name: "My Custom".to_string(),
            ui: built_in_presets()[0].ui.clone(),
            term: built_in_presets()[0].term.clone(),
        };
        state.custom.push(custom.clone());

        save(&dir, &state).unwrap();
        let back = load(&dir);
        assert_eq!(back, state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_falls_back_to_default() {
        let dir = tempdir("corrupt");
        std::fs::write(themes_file_path(&dir), b"{not valid json").unwrap();
        let s = load(&dir);
        assert_eq!(s.active, "default");
        assert!(s.custom.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_preset_works() {
        assert!(find_preset("default").is_some());
        assert!(find_preset("dracula").is_some());
        assert!(find_preset("tokyo-night").is_some());
        assert!(find_preset("catppuccin-latte").is_some());
        assert!(find_preset("catppuccin-frappe").is_some());
        assert!(find_preset("catppuccin-macchiato").is_some());
        assert!(find_preset("catppuccin-mocha").is_some());
        assert!(find_preset("nope").is_none());
    }
}
