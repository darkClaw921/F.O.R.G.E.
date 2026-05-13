# tmux-web/src/attention.rs

Attention-watcher для tmux-сессий: фоновый цикл, который раз в 1.5с обходит все tmux-сессии активного проекта, захватывает содержимое pane через crate::tmux::capture_pane, применяет детектор Claude permission prompt и записывает финальные флаги needs_attention в shared AttentionState. С Phase 2 forge-bjm включает обязательную дедупликацию между сессиями одной session-group и сессиями с идентичным pane_hash.

ОСНОВНЫЕ ЭЛЕМЕНТЫ:

- pub struct AttentionState — Arc<RwLock<HashMap<session_name, bool>>>. Дёшево клонируется, дешёвый snapshot() возвращает owned copy. set(name, flag) пишет финальный флаг (после дедупа). Используется axum-хендлерами /api/attention и broadcast'ом в WS.

- pub fn detect_claude_prompt(pane: &str) -> bool — строгий AND-детектор: одновременно требует '❯ 1. Yes', '2. Yes,', '3. No'. Покрывает варианты edit/file-create/bash prompt'ов Claude Code. Короткие маркеры выбраны сознательно для покрытия разных UI-режимов.

- pub async fn watcher_loop(attention: Arc<AttentionState>) — основной цикл (sleep 1500ms → list_sessions → capture_pane по каждой → детектор → дедуп → set). Никогда не завершается штатно. Сбой list_sessions/capture_pane не валит loop (unwrap_or_default).

  Иттерация состоит из трёх шагов:
  1) Сбор Vec<SessionAttention> — для каждой сессии: name, id, attached, session_group, pane_hash, detected. На этом шаге эмитится диагностический tracing::debug!('attention check') с полями session/group/pane_hash/detected/pane_len.
  2) Дедупликация через deduplicate_attention(&collected).
  3) Запись финальных флагов attention.set(name, flag) для каждой сессии.

- struct SessionAttention (private) — снимок состояния одной сессии для дедупа в одной итерации. Поля: name, id, attached, session_group, pane_hash, detected.

- fn hash_pane(pane: &str) -> u64 — DefaultHasher по содержимому pane. Стабилен в рамках одного процесса. Не криптостоек, не нужно: используется только для эквивалентности 'один и тот же текст → один хэш'.

- fn deduplicate_attention(items: &[SessionAttention]) -> Vec<(String, bool)> — чистая функция, нормализует флаги. Алгоритм:
  * Union-find по двум осям: pane_hash (точное совпадение содержимого) и session_group (linked-сессии tmux могут расходиться по cursor-blink, но логически делят работу).
  * В каждой объединённой группе: если ни одной detected=true → все остаются false; если хотя бы одна detected=true → выбирается primary через pick_primary, у него флаг true, у остальных false (даже если их детектор сработал).
  Это устраняет 'оранжевое отображение всей группы' — root cause баг-репорта forge-bjm.

- fn pick_primary(items, members) -> Option<usize> — выбирает primary среди detected=true членов группы:
  1) attached>0 имеет приоритет (пользователь реально смотрит на эту сессию);
  2) среди attached>0 (или среди всех если все detached) — наибольший session_id лексикографически: свежее созданная сессия предпочтительнее;
  3) fallback: лексикографически наибольшее имя.
  Полностью детерминирована — одни и те же входы → один и тот же primary.

ТЕСТЫ (Phase 2): #[cfg(test)] mod tests содержит 8 новых юнит-тестов дедуп-логики (dedup_same_pane_hash_keeps_only_primary, dedup_different_pane_hash_no_grouping, dedup_attached_wins_over_detached, dedup_same_group_unifies_even_with_different_pane_hash, dedup_no_detection_keeps_all_false, dedup_three_detached_picks_largest_id, dedup_empty_input_returns_empty, dedup_single_detected_session_unchanged) + hash_pane_is_deterministic_and_collision_free_for_distinct_inputs. Helper mk_session(name, attached, group, pane_hash, detected) для краткости фикстур. Тесты не используют моки tmux — оперируют SessionAttention напрямую.

ЗАВИСИМОСТИ:
- crate::tmux::list_sessions, crate::tmux::capture_pane — источник данных.
- crate::tmux::SessionInfo — содержит поле session_group, добавленное в Phase 1.1.
- tokio::sync::RwLock — async-замок для AttentionState.
- tracing — debug-логирование диагностики.

ДИАГНОСТИКА: при RUST_LOG=tmux_web::attention=debug каждые 1.5с в логах появляется строка 'attention check' для каждой сессии с полями session/group/pane_hash/detected/pane_len — это ключевой инструмент для воспроизведения и подтверждения причины ложно-позитивных срабатываний (Phase 1.2).
