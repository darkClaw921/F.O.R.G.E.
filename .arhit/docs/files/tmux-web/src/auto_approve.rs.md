# tmux-web/src/auto_approve.rs

Модуль авто-апрува интерактивных prompt'ов Claude Code в tmux-сессиях (фича tw-pg5).

## Зачем
attention::detect_claude_prompt только ЗАМЕЧАЕТ prompt (оранжевое свечение карточки), нажимать «1» приходилось человеку. Модуль закрывает разрыв: фоновый воркер сам отправляет выбор через tmux::send_key, сессия не простаивает на «Do you want to proceed?».

## Инвариант opt-in
UserSettings::auto_approve_enabled по умолчанию false. При выключенном флаге run() не делает НИ ОДНОГО tmux-вызова — проверка флага стоит до list_sessions. Тот же инвариант, что у next_step_enabled.

## API
- enum PromptKind { Permission3, Permission2, AllowDeny, AllowBroad, Question, Plan }
- classify_prompt(pane) -> Option<PromptKind> — чистая функция, нормализует панель через attention::normalize_ws (word-wrap footer'ов), порядок проверок: Plan/Question (по footer) → Chrome Allow → Yes-permission.
- decide(kind, pane, &UserSettings) -> Option<&'static str> — какую цифру жать; None = не трогать.
- is_dangerous(pane, extra) — блоклист (приватная).
- run(state: AppState) — воркер, тик 2с.
- DEFAULT_DANGEROUS — встроенный блоклист (rm -rf, git push --force, drop table, | sh, dd if=, chmod 777 и т.п.).

## Таблица форм prompt'ов
| Kind | Маркеры | Выбор |
| Permission3 | '❯ 1. Yes' + '2. Yes,' + '3. No' | 1, broad → 2 |
| Permission2 | '❯ 1. Yes' + '2. No' | всегда 1 |
| AllowDeny | '❯ 1. Allow' + '2. Deny' | всегда 1 |
| AllowBroad | '❯ 1. Allow' + '2. Allow all actions on' | 1, broad → 2 |
| Question | 'Enter to select' + '↑/↓ to navigate' | 1, если auto_approve_questions |
| Plan | 'Enter to select' + 'Tab/Arrow keys to navigate' | НИКОГДА |

Ключевое правило broad-режима: второй пункт жмётся только там, где он расширяет разрешение (Yes-and-dont-ask / Allow-all). В двухопционных Yes/No и Allow/Deny второй пункт — отказ, поэтому Permission2 и AllowDeny вынесены в отдельные варианты.

Утверждение плана (ExitPlanMode) не подтверждается никогда и настройкой не управляется.

## Защиты
- Стабильность: нажатие только если один и тот же hash_pane наблюдался два тика подряд (TUI рисуется кадрами).
- Анти-луп: максимум MAX_ATTEMPTS=2 нажатия за серию «prompt непрерывно распознан». Первое — цифра, второе — цифра + Enter. Счётчик обнуляется, когда classify_prompt вернул None (prompt ушёл), а НЕ при смене хэша панели: нажатая цифра сама печатается на экран и меняет хэш — привязка к хэшу давала бы бесконечную очередь '1111…' в сессии, игнорирующей ввод (обнаружено живой проверкой).
- Prune карты состояний по исчезнувшим сессиям.

## Зависимости
crate::attention::normalize_ws, crate::user_settings::UserSettings, crate::AppState (нужен user_settings — потому воркер отдельный, а не внутри attention::watcher_loop, который намеренно берёт только Arc<AttentionState>), crate::tmux::{list_sessions, capture_pane, send_key}.

Спавнится в main.rs: tokio::spawn(auto_approve::run(app_state.clone())) рядом с attention::watcher_loop. UI-тумблеры — static/js/settings/interface-tab.js. Тесты — 12 юнит-тестов в модуле (фикстуры всех форм, broad-правило, блоклист, word-wrap).
