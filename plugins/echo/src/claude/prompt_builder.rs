//! Сборка финального prompt'а для Claude CLI.
//!
//! Подмешивает в один текст:
//! - содержимое релевантных tmux-pane'ов (`capture_pane_full`),
//! - релевантные memories (global_day за вчера + project memories для project_id),
//! - сам user_text.
//!
//! После Phase 4 (`remove-projects-concept`) секция `[projects]` удалена —
//! HostApi больше не предоставляет список проектов. `project_id` в
//! `CtxOpts` остаётся как непрозрачный ярлык для фильтрации memories
//! внутри SQLite (Echo сам управляет этим soft-FK).
//!
//! ## Стратегия "не упасть, если что-то не доступно"
//!
//! Любая capture-pane ошибка для одной сессии не должна обрушить весь prompt
//! — мы её пропускаем и продолжаем со следующей. Если memories пуст — секция
//! опускается полностью.
//!
//! ## Лимит размера
//!
//! Capture-lines дефолтно = 200 (см. [`CtxOpts::default`]). Это даёт ~10-30
//! КБ на сессию в худшем случае. При 5+ сессиях prompt может вырасти до
//! сотен КБ; для Phase 3 это допустимо (Claude переваривает ~200K input
//! tokens), для Phase 6 в `config` появится `capture_lines_total_cap`.

use std::fmt::Write;

use echo_host_api::HostApi;

use crate::db::repo::memories;
use crate::db::Db;

/// Опции построения контекста.
#[derive(Debug, Clone)]
pub struct CtxOpts {
    /// Подмешивать ли `capture_pane_full` сессий.
    pub include_pane_capture: bool,
    /// Фильтр сессий по project_id (memories тоже фильтруются по нему).
    pub project_id: Option<String>,
    /// Подмешивать ли memories.
    pub include_memories: bool,
    /// Сколько строк истории захватывать.
    pub capture_lines: i32,
    /// Если задан — захватывать только сессии с именами из этого списка.
    /// `None` → все сессии (с применённым project-filter'ом если в плагине
    /// будет такая логика; в Phase 3 — все сессии хоста).
    pub session_filter: Option<Vec<String>>,
}

impl Default for CtxOpts {
    fn default() -> Self {
        Self {
            include_pane_capture: true,
            project_id: None,
            include_memories: true,
            capture_lines: 200,
            session_filter: None,
        }
    }
}

/// Собирает финальный текст prompt'а.
///
/// Структура (секции опускаются если данных нет):
///
/// ```text
/// [system_context]
///
/// [tmux_sessions]
/// ## session: <name>
/// <pane content>
/// ---
/// ## session: <other>
/// ...
///
/// [memories]
/// ### Global (yesterday)
/// <content>
///
/// ### Project <project_id>
/// <content>
///
/// [user_message]
/// <user_text>
/// ```
///
/// Если HostApi или Db вернули ошибку — конкретная секция пропускается с
/// warn-log'ом, но функция в целом возвращает Ok.
pub async fn build(
    user_text: &str,
    opts: &CtxOpts,
    host: &dyn HostApi,
    db: &Db,
) -> anyhow::Result<String> {
    let mut out = String::with_capacity(4096);
    out.push_str("[system_context]\n");
    out.push_str(
        "You are F.O.R.G.E. Echo — an embedded chat assistant integrated with the developer's tmux \
sessions and project memories. Use the context below to ground your answer.\n",
    );

    // -- tmux sessions --
    if opts.include_pane_capture {
        match host.list_sessions().await {
            Ok(sessions) => {
                let filtered: Vec<_> = sessions
                    .into_iter()
                    .filter(|s| match &opts.session_filter {
                        Some(allow) => allow.iter().any(|n| n == &s.name),
                        None => true,
                    })
                    .collect();
                if !filtered.is_empty() {
                    out.push_str("\n[tmux_sessions]\n");
                    for s in filtered {
                        match host.capture_pane_full(&s.name, opts.capture_lines).await {
                            Ok(pane) if !pane.trim().is_empty() => {
                                let _ = writeln!(
                                    out,
                                    "## session: {} ({} windows)\n{}\n---",
                                    s.name, s.windows, pane.trim_end()
                                );
                            }
                            Ok(_) => {
                                tracing::debug!(session = %s.name, "prompt_builder: empty pane, skip");
                            }
                            Err(e) => {
                                tracing::warn!(
                                    session = %s.name,
                                    error = %e,
                                    "prompt_builder: capture_pane_full failed, skipping session"
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "prompt_builder: list_sessions failed");
            }
        }
    }

    // -- memories --
    if opts.include_memories {
        let mut mem_section = String::new();

        // global_day за вчера (UTC).
        let yesterday = (chrono::Utc::now() - chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        match memories::list(db, Some(memories::MemoryScope::GlobalDay), None, Some(&yesterday)).await {
            Ok(list) => {
                for m in list {
                    let _ = writeln!(
                        mem_section,
                        "### Global ({yesterday})\n{}\n",
                        m.content.trim_end()
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "prompt_builder: memories.list(global_day) failed");
            }
        }

        // project-scope memories.
        if let Some(pid) = &opts.project_id {
            match memories::list(db, Some(memories::MemoryScope::Project), Some(pid), None).await {
                Ok(list) => {
                    for m in list {
                        let _ = writeln!(
                            mem_section,
                            "### Project {pid}\n{}\n",
                            m.content.trim_end()
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, project = %pid, "prompt_builder: memories.list(project) failed");
                }
            }
        }

        if !mem_section.is_empty() {
            out.push_str("\n[memories]\n");
            out.push_str(&mem_section);
        }
    }

    // -- user message --
    out.push_str("\n[user_message]\n");
    out.push_str(user_text);
    if !user_text.ends_with('\n') {
        out.push('\n');
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use echo_host_api::SessionInfo;

    /// Mock HostApi для тестов.
    struct MockHost {
        sessions: Vec<SessionInfo>,
        // Имя сессии → выдача capture_pane_full. Если "ERR" — вернёт Err.
        pane_data: std::collections::HashMap<String, String>,
    }

    #[async_trait]
    impl HostApi for MockHost {
        async fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
            Ok(self.sessions.clone())
        }
        async fn capture_pane_full(&self, session: &str, _lines: i32) -> anyhow::Result<String> {
            match self.pane_data.get(session) {
                Some(s) if s == "ERR" => anyhow::bail!("simulated capture error"),
                Some(s) => Ok(s.clone()),
                None => Ok(String::new()),
            }
        }
        fn auth_token(&self) -> Option<String> {
            None
        }
    }

    async fn fresh_db() -> Db {
        let db = Db::open_memory().await.unwrap();
        db.migrate().await.unwrap();
        db
    }

    fn make_host(sessions: &[(&str, &str)]) -> MockHost {
        let mut pane_data = std::collections::HashMap::new();
        let session_infos = sessions
            .iter()
            .map(|(name, pane)| {
                pane_data.insert(name.to_string(), pane.to_string());
                SessionInfo {
                    name: name.to_string(),
                    windows: 1,
                    panes: 1,
                }
            })
            .collect();
        MockHost {
            sessions: session_infos,
            pane_data,
        }
    }

    #[tokio::test]
    async fn build_includes_user_text_always() {
        let host = make_host(&[]);
        let db = fresh_db().await;
        let p = build("Hello world", &CtxOpts::default(), &host, &db).await.unwrap();
        assert!(p.contains("[user_message]"));
        assert!(p.contains("Hello world"));
    }

    #[tokio::test]
    async fn build_includes_pane_capture_for_each_session() {
        let host = make_host(&[("dev", "$ ls\nfoo\nbar"), ("logs", "tail output")]);
        let db = fresh_db().await;
        let p = build("hi", &CtxOpts::default(), &host, &db).await.unwrap();
        assert!(p.contains("[tmux_sessions]"));
        assert!(p.contains("## session: dev"));
        assert!(p.contains("foo"));
        assert!(p.contains("## session: logs"));
        assert!(p.contains("tail output"));
    }

    #[tokio::test]
    async fn build_skips_failed_capture_but_continues() {
        let host = make_host(&[("good", "ok"), ("broken", "ERR")]);
        let db = fresh_db().await;
        let p = build("hi", &CtxOpts::default(), &host, &db).await.unwrap();
        // "good" есть, "broken" опущена, секция всё ещё присутствует.
        assert!(p.contains("## session: good"));
        assert!(!p.contains("## session: broken"));
    }

    #[tokio::test]
    async fn build_skips_empty_pane() {
        let host = make_host(&[("empty", "   \n  \n")]);
        let db = fresh_db().await;
        let p = build("hi", &CtxOpts::default(), &host, &db).await.unwrap();
        // session header не должен появляться для пустого pane
        assert!(!p.contains("## session: empty"));
    }

    #[tokio::test]
    async fn build_filters_sessions_by_session_filter() {
        let host = make_host(&[("dev", "X"), ("logs", "Y")]);
        let db = fresh_db().await;
        let opts = CtxOpts {
            session_filter: Some(vec!["dev".into()]),
            ..CtxOpts::default()
        };
        let p = build("hi", &opts, &host, &db).await.unwrap();
        assert!(p.contains("## session: dev"));
        assert!(!p.contains("## session: logs"));
    }

    #[tokio::test]
    async fn build_omits_memory_section_when_empty() {
        let host = make_host(&[]);
        let db = fresh_db().await;
        let p = build("hi", &CtxOpts::default(), &host, &db).await.unwrap();
        assert!(!p.contains("[memories]"), "no memories — секция должна отсутствовать");
    }

    #[tokio::test]
    async fn build_includes_global_yesterday_memory() {
        let host = make_host(&[]);
        let db = fresh_db().await;
        let yesterday = (chrono::Utc::now() - chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        memories::upsert(
            &db,
            memories::MemoryScope::GlobalDay,
            None,
            Some(&yesterday),
            "Yesterday I learned X",
            "auto",
        )
        .await
        .unwrap();
        let p = build("hi", &CtxOpts::default(), &host, &db).await.unwrap();
        assert!(p.contains("[memories]"));
        assert!(p.contains("Yesterday I learned X"));
    }

    #[tokio::test]
    async fn build_includes_project_memory_when_project_id_set() {
        let host = make_host(&[]);
        let db = fresh_db().await;
        memories::upsert(
            &db,
            memories::MemoryScope::Project,
            Some("p1"),
            None,
            "Project notes for p1",
            "manual",
        )
        .await
        .unwrap();
        // Опции с project_id.
        let opts = CtxOpts {
            project_id: Some("p1".into()),
            ..CtxOpts::default()
        };
        let p = build("hi", &opts, &host, &db).await.unwrap();
        assert!(p.contains("Project notes for p1"));
        // Без project_id project-memory не должна попадать.
        let p2 = build("hi", &CtxOpts::default(), &host, &db).await.unwrap();
        assert!(!p2.contains("Project notes for p1"));
    }

    #[tokio::test]
    async fn build_respects_disable_pane_and_memories() {
        let host = make_host(&[("dev", "X")]);
        let db = fresh_db().await;
        let yesterday = (chrono::Utc::now() - chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        memories::upsert(
            &db,
            memories::MemoryScope::GlobalDay,
            None,
            Some(&yesterday),
            "y",
            "auto",
        )
        .await
        .unwrap();
        let opts = CtxOpts {
            include_pane_capture: false,
            include_memories: false,
            ..CtxOpts::default()
        };
        let p = build("hi", &opts, &host, &db).await.unwrap();
        assert!(!p.contains("[tmux_sessions]"));
        assert!(!p.contains("[memories]"));
        assert!(p.contains("hi"));
    }
}
