# tmux-web/static/js/sessions/sessions.js

Список tmux-сессий: загрузка, поллинг, рендер карточек сайдбара.

## Ключевые функции

- fetchSessions() — Promise.all([fetch('/api/sessions'), fetchNextSteps()]) → state.sessions → renderSidebar() + syncHomeVisibility(). Ошибка next-steps не ломает основной список (fetchNextSteps глотает свои ошибки). startPolling() зовёт каждые 3с.
- fetchNextSteps() — GET /api/echo/next-steps → state.nextSteps (map session → { content }). Зовётся из fetchSessions (poll 3с) и напрямую из echo/ws.js по WS-событию NextStepEvent (мгновенная реакция).
- buildSessionItem(s) — карточка сессии.
- isNextStepEnabled() — приватный предикат гейта (см. ниже).

## Три независимых индикатора на карточке

1. **.needs-attention** — ОРАНЖЕВЫЙ (sidebar.css:104). Источник: SessionDto.needs_attention (attention.rs). Claude показал prompt и ждёт ответа. Настройкой не управляется.
2. **.has-next-step** — ГОЛУБОЕ пульсирующее свечение (#4fd0e7, sidebar.css:216-247). Источник: наличие ключа в state.nextSteps. Эпизод завершён и есть готовое предложение что делать дальше. Взаимоисключающе с (1): сессии с needs_attention исключены из idle_snapshot на бэкенде.
3. **.claude-spark ✶** — синий спарк (sidebar.css:180). Источник: SessionDto.is_generating. Claude печатает прямо сейчас. Tooltip показывает generating_since_secs.

Классы снимаются автоматически при перерендере.

## Гейт фичи «Следующий шаг»

isNextStepEnabled() читает state.userSettings.next_step_enabled лениво (строгое '=== true'; null/undefined → выключено, совпадает с backend-дефолтом false — фича opt-in). Предикат намеренно ПРИВАТНЫЙ, а не импорт из settings/interface-tab.js: список сессий не должен зависеть от view-модуля вкладки настроек. Тот же приём, что isEchoNotificationsEnabled в echo/notifications.js.

При выключенной фиче fetchNextSteps() ставит state.nextSteps = {} и выходит БЕЗ HTTP-запроса. Один гейт гасит и свечение, и hover-попап (sessions/next-step-popup.js читает тот же state.nextSteps) мгновенно, не дожидаясь пока бэкенд дочистит предложения broadcast'ом. Отдельная проверка в buildSessionItem не нужна — при пустом state.nextSteps условие само не срабатывает.

Ранний return безопасен для Promise.all в fetchSessions: функция async, читается только результат [0].

Известная шероховатость: bootstrap.js зовёт fetchSessions() до fetchUserSettings(), поэтому первый fetchNextSteps всегда гейтится; при включённой фиче свечение появится на тик позже (3с) или сразу по WS.

См. [[interface-settings-toggles]].
