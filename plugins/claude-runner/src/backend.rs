//! Выбор LLM-бэкенда раннера: локальный `claude -p`, локальный `kimi -p` или
//! облачный OpenAI-совместимый провайдер (chat/completions по SSE).
//!
//! ## Зачем отдельный модуль
//!
//! Плагины (echo, meet, bitrix, vk, telegram) раньше жёстко знали только
//! `claude -p`. Теперь у каждого плагина в конфиге появляется выбор
//! [`LlmBackend`], и чтобы шесть плагинов не дублировали резолв «какой
//! транспорт собрать из полей конфига», вся логика живёт здесь, в листовом
//! крейте:
//!
//! ```text
//! config.llm_backend ─┬─ Claude ──► Transport::Cli { kind: Claude, path }
//!                     ├─ Kimi   ──► Transport::Cli { kind: Kimi,   path }
//!                     └─ Cloud  ──► Transport::Cloud { base_url, key, model }
//! ```
//!
//! ## Облако — НЕ новый HTTP-клиент
//!
//! Транспорт [`Transport::Cloud`] переиспользует [`forge_voice::cloud`]:
//! polza.ai-совместимый агрегатор уже умеет авторизацию заголовком, обрезку
//! тел отказов и таймауты соединения. `forge-voice` — листовое ядро и никогда
//! не зависит от `forge-claude-runner`, поэтому зависимость односторонняя и
//! цикла по построению невозможно. Правило зафиксировано комментарием в
//! `Cargo.toml` рядом с зависимостью.

use std::path::PathBuf;

/// Локальный CLI-движок: `claude -p` или `kimi -p`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CliKind {
    /// Anthropic Claude CLI (`claude -p --output-format stream-json`).
    #[default]
    Claude,
    /// Kimi CLI (`kimi -p --output-format stream-json`, print-режим).
    Kimi,
}

impl CliKind {
    /// Все варианты — для выборов в конфигах и UI.
    pub const ALL: &'static [&'static str] = &["claude", "kimi"];

    /// Разбор строки конфига. Нераспознанное → `None` (конфиг-валидатор
    /// покажет ошибку, дефолт тут не выбираем).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "claude" => Some(Self::Claude),
            "kimi" => Some(Self::Kimi),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Kimi => "kimi",
        }
    }

    /// Дефолтный путь установки CLI в $HOME.
    #[must_use]
    pub fn default_path(self) -> PathBuf {
        let bin = match self {
            Self::Claude => "claude",
            Self::Kimi => "kimi",
        };
        // Разворачиваем ~ вручную: конфиги хранят пути строками и
        // `shellexpand` в дереве нет.
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            p.push(".local");
            p.push("bin");
            p.push(bin);
            p
        } else {
            PathBuf::from(bin)
        }
    }
}

/// Трёхпозиционный выбор LLM-бэкенда из конфигов плагинов (по образцу
/// `SttBackend`/`SummaryBackend` у meet: тот же `ALL`/`parse`/`as_str`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmBackend {
    /// Локальный `claude -p` (историческое поведение).
    #[default]
    Claude,
    /// Локальный `kimi -p` (Kimi CLI, print-режим).
    Kimi,
    /// Облачный OpenAI-совместимый провайдер (`POST {base}/chat/completions`,
    /// SSE). Требует настроенного ключа.
    Cloud,
}

impl LlmBackend {
    /// Все варианты — для Choice-полей конфигов.
    pub const ALL: &'static [&'static str] = &["claude", "kimi", "cloud"];

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "claude" => Some(Self::Claude),
            "kimi" => Some(Self::Kimi),
            "cloud" => Some(Self::Cloud),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Kimi => "kimi",
            Self::Cloud => "cloud",
        }
    }

    /// Использует ли вариант локальный CLI (нужен `cli_available()`).
    #[must_use]
    pub fn is_local(self) -> bool {
        matches!(self, Self::Claude | Self::Kimi)
    }

    /// Соответствующий локальный CLI. `None` для [`Self::Cloud`].
    #[must_use]
    pub fn cli_kind(self) -> Option<CliKind> {
        match self {
            Self::Claude => Some(CliKind::Claude),
            Self::Kimi => Some(CliKind::Kimi),
            Self::Cloud => None,
        }
    }
}

/// Облачный транспорт: куда и с каким ключом слать chat/completions.
///
/// `api_key` — строка из конфига; внутри облачной ветки она заворачивается в
/// [`forge_voice::cloud::SecretKey`], чтобы ключ не утёк в логи через `Debug`.
///
/// Собственный `Debug` — маска ключа: эта структура проходит через логи
/// плагинов, и вывод ключа «на всякий случай» — это утечка, а не диагностика.
#[derive(Clone, Default)]
pub struct CloudTransportCfg {
    /// База API без завершающего слэша (дефолт — polza.ai).
    pub base_url: String,
    /// Ключ. Пустой — облако не настроено, run откажет с понятной ошибкой.
    pub api_key: String,
    /// Модель (дефолт — [`forge_voice::cloud::DEFAULT_SUMMARY_MODEL`]).
    pub model: String,
}

impl std::fmt::Debug for CloudTransportCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudTransportCfg")
            .field("base_url", &self.base_url)
            .field("api_key", &forge_voice::cloud::SecretKey::new(&self.api_key))
            .field("model", &self.model)
            .finish()
    }
}

impl CloudTransportCfg {
    /// Настроено ли облако: резолвится ли непустой ключ.
    ///
    /// Модель не обязательна: пустая — будет дефолт
    /// [`forge_voice::cloud::DEFAULT_SUMMARY_MODEL`].
    #[must_use]
    pub fn configured(&self) -> bool {
        !self.resolve_api_key().is_empty()
    }

    /// Ключ с fallback-цепочкой (по убыванию приоритета):
    ///
    /// 1. `api_key` из конфига плагина (`llm_cloud_api_key`);
    /// 2. env `FORGE_LLM_CLOUD_API_KEY` — общий ключ для LLM-запросов;
    /// 3. env `FORGE_MEET_POLZA_KEY` — существующее имя ключа polza.ai в
    ///    проекте (meet использует его для live-тестов);
    /// 4. `polza_api_key` из `~/.config/forge/meet.toml` — первичное место
    ///    хранения polza-ключа у пользователя forge: из коробки облачный
    ///    бэкенд работает без дублирования ключа в каждый конфиг плагина.
    #[must_use]
    pub fn resolve_api_key(&self) -> String {
        resolve_api_key_chain(&self.api_key, std::env::var("FORGE_LLM_CLOUD_API_KEY").ok().as_deref(), std::env::var("FORGE_MEET_POLZA_KEY").ok().as_deref(), polza_key_from_meet_toml().as_deref())
    }

    /// Модель с fallback на дефолт polza.ai.
    #[must_use]
    pub fn resolve_model(&self) -> String {
        let m = self.model.trim();
        if m.is_empty() {
            forge_voice::cloud::DEFAULT_SUMMARY_MODEL.to_string()
        } else {
            m.to_string()
        }
    }
}

/// Чистая цепочка резолва ключа — отдельно от чтения env/файла, чтобы
/// тестировать без окружения.
fn resolve_api_key_chain(
    cfg_key: &str,
    env_llm: Option<&str>,
    env_meet: Option<&str>,
    meet_toml: Option<&str>,
) -> String {
    for candidate in [cfg_key.trim(), env_llm.unwrap_or("").trim(), env_meet.unwrap_or("").trim(), meet_toml.unwrap_or("").trim()] {
        if !candidate.is_empty() {
            return candidate.to_string();
        }
    }
    String::new()
}

/// Вытащить `polza_api_key` из содержимого meet.toml. Построчный разбор
/// `ключ = "значение"` — без toml-зависимости: раннер остаётся листовым
/// крейтом с минимальным деревом.
fn parse_polza_key_from_toml_content(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("polza_api_key") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let value = rest.trim().trim_matches('"').trim_matches('\'');
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// Ключ polza.ai из `~/.config/forge/meet.toml` (если файл есть и читается).
fn polza_key_from_meet_toml() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home).join(".config/forge/meet.toml");
    let content = std::fs::read_to_string(path).ok()?;
    parse_polza_key_from_toml_content(&content)
}

/// Чем исполнять run: локальный CLI одного из видов или облако.
#[derive(Debug, Clone)]
pub enum Transport {
    Cli { kind: CliKind, path: PathBuf },
    Cloud(CloudTransportCfg),
}

impl Transport {
    /// Локальный Claude CLI (эквивалент прежнего `ClaudeRunner::new(path, n)`).
    #[must_use]
    pub fn claude_cli(path: PathBuf) -> Self {
        Self::Cli { kind: CliKind::Claude, path }
    }

    /// Явно ли транспорт облачный и настроен ли он (для healthz-подобных
    /// ответов и 409-сообщений chat-analysis).
    #[must_use]
    pub fn cloud_ready(&self) -> bool {
        matches!(self, Self::Cloud(cfg) if cfg.configured())
    }
}

/// Единая точка резолва «конфиг → runner» для всех плагинов.
///
/// `claude_path`/`kimi_path` — пути из конфигов плагинов (у Cloud не читаются).
/// Облачный транспорт с пустым ключом собирается как есть: `new()` не падает,
/// а первый run ответит понятной ошибкой — ровно как с отсутствующим CLI.
#[must_use]
pub fn runner_for(
    backend: LlmBackend,
    claude_path: PathBuf,
    kimi_path: PathBuf,
    cloud: CloudTransportCfg,
    max_parallel: usize,
) -> crate::ClaudeRunner {
    let transport = match backend {
        LlmBackend::Claude => Transport::Cli { kind: CliKind::Claude, path: claude_path },
        LlmBackend::Kimi => Transport::Cli { kind: CliKind::Kimi, path: kimi_path },
        LlmBackend::Cloud => Transport::Cloud(cloud),
    };
    crate::ClaudeRunner::from_transport(transport, max_parallel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_kind_round_trips() {
        for s in CliKind::ALL {
            let k = CliKind::parse(s).unwrap();
            assert_eq!(k.as_str(), *s);
        }
        assert_eq!(CliKind::parse("CLAUDE"), Some(CliKind::Claude));
        assert!(CliKind::parse("openai").is_none());
        assert_eq!(CliKind::default(), CliKind::Claude);
    }

    #[test]
    fn cli_kind_default_path_under_home() {
        if std::env::var_os("HOME").is_none() {
            return;
        }
        let p = CliKind::Kimi.default_path();
        assert!(p.ends_with(".local/bin/kimi"), "{p:?}");
        let p = CliKind::Claude.default_path();
        assert!(p.ends_with(".local/bin/claude"), "{p:?}");
    }

    #[test]
    fn llm_backend_round_trips() {
        for s in LlmBackend::ALL {
            let b = LlmBackend::parse(s).unwrap();
            assert_eq!(b.as_str(), *s);
        }
        assert_eq!(LlmBackend::parse(" Cloud "), Some(LlmBackend::Cloud));
        assert!(LlmBackend::parse("local").is_none());
        assert_eq!(LlmBackend::default(), LlmBackend::Claude);
        assert!(LlmBackend::Claude.is_local());
        assert!(LlmBackend::Kimi.is_local());
        assert!(!LlmBackend::Cloud.is_local());
        assert_eq!(LlmBackend::Kimi.cli_kind(), Some(CliKind::Kimi));
        assert_eq!(LlmBackend::Cloud.cli_kind(), None);
    }

    #[test]
    fn cloud_cfg_configured_needs_key_only() {
        // Только чистая цепочка — без env/файла, чтобы тест был
        // воспроизводимым в любом окружении.
        assert!(resolve_api_key_chain("", None, None, None).is_empty());
        assert_eq!(resolve_api_key_chain("  ", None, None, None), "");
        assert_eq!(
            resolve_api_key_chain("pza_cfg", Some("pza_env"), None, None),
            "pza_cfg",
            "ключ конфига побеждает env"
        );
        assert_eq!(
            resolve_api_key_chain("", Some("pza_env_llm"), Some("pza_env_meet"), None),
            "pza_env_llm",
            "общий env-ключ побеждает meet-ключ"
        );
        assert_eq!(
            resolve_api_key_chain("", None, Some("pza_env_meet"), Some("pza_toml")),
            "pza_env_meet",
            "env побеждает meet.toml"
        );
        assert_eq!(
            resolve_api_key_chain("", None, None, Some("pza_toml")),
            "pza_toml",
            "meet.toml — последний fallback"
        );
        // configured() зависит от окружения; в чистом виде (без ключей
        // нигде) дефолтный cfg не настроен, а с ключом — настроен.
        if std::env::var_os("FORGE_LLM_CLOUD_API_KEY").is_none()
            && std::env::var_os("FORGE_MEET_POLZA_KEY").is_none()
            && polza_key_from_meet_toml().is_none()
        {
            assert!(!CloudTransportCfg::default().configured());
        }
        let mut cfg = CloudTransportCfg::default();
        cfg.api_key = "pza_test".into();
        assert!(cfg.configured(), "модель не обязательна — есть дефолт");
    }

    #[test]
    fn cloud_cfg_resolve_model_falls_back_to_default() {
        let cfg = CloudTransportCfg::default();
        assert_eq!(cfg.resolve_model(), forge_voice::cloud::DEFAULT_SUMMARY_MODEL);
        let cfg = CloudTransportCfg { model: " custom/model ".into(), ..Default::default() };
        assert_eq!(cfg.resolve_model(), "custom/model");
    }

    #[test]
    fn parse_polza_key_from_toml_content_picks_key() {
        let content = r#"
# комментарий
polza_api_key = "pza_FROMFILE123"
summary_backend = "cloud"
"#;
        assert_eq!(parse_polza_key_from_toml_content(content).as_deref(), Some("pza_FROMFILE123"));
        assert!(parse_polza_key_from_toml_content("# polza_api_key = \"x\"").is_none());
        assert!(parse_polza_key_from_toml_content("other = \"1\"").is_none());
        assert!(parse_polza_key_from_toml_content("polza_api_key = \"\"").is_none());
    }

    #[test]
    fn runner_for_picks_transport() {
        let r = runner_for(
            LlmBackend::Kimi,
            PathBuf::from("/no/claude"),
            PathBuf::from("/no/kimi"),
            CloudTransportCfg::default(),
            2,
        );
        assert_eq!(r.available_permits(), 2);
    }
}
