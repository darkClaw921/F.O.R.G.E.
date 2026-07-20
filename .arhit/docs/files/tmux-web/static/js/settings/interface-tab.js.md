# tmux-web/static/js/settings/interface-tab.js

Вкладка «Интерфейс» модалки настроек — тумблеры двух opt-in фич интерфейса.

## Назначение

Экспортирует единственную функцию buildInterfaceForm(settings, onSaved) — DOM-фабрику fieldset'а с двумя чекбоксами. Контракт совпадает с todo-tab.js: принимает снапшот настроек (может быть null/{}), возвращает готовый DOM-узел, зовёт onSaved(updated) после успешного PATCH.

## Настройки (мэппинг 1:1 на UserSettings в tmux-web/src/user_settings.rs)

- cmd_hints_enabled (bool, default false) — подсказки хоткеев при удержании ⌘ (static/hotkeys.js). Удержание Command >200мс рисует на кликабельных элементах жёлтые бейджи с буквенными кодами; набор кода даёт focus()+click().
- next_step_enabled (bool, default false) — фича «Следующий шаг» ЦЕЛИКОМ: голубое свечение карточек сессий (.has-next-step), hover-попап и генерация подсказок воркером Echo через Claude CLI.

## Критично: дефолты выключены

Обе фичи opt-in — при нулевой конфигурации выключены. Поэтому чекбоксы инициализируются строго '=== true'. Идиома '!== false' (echo/settings.js:105 для echo_notifications_enabled) здесь НЕПРИМЕНИМА: у той настройки дефолт включённый, копирование инвертировало бы наш. Это намеренное сужение инварианта tw-z6l «нулевая конфигурация = поведение как до фичи», см. шапку user_settings.rs.

## Save

Шлёт через updateUserSettings() только свои два поля — PATCH применяет лишь Some(..)-варианты, соседние настройки (echo_*, todo_*) не затрагиваются. Ошибка показывается в .notify-error, успех — в .interface-save-ok на 2 сек.

## Зависимости

- Импортирует updateUserSettings из ./user-settings-api.js (optimistic update + rollback, пишет в state.userSettings).
- Монтируется в settings/modal.js::renderInterfacePanel (ленивый рендер при первом показе вкладки).
- CSS не требует: переиспользует .notify-fieldset / .notify-hint / .notify-error / .notify-actions / .modal-check из notifications.css и modals.css.

## Как настройки доезжают до потребителей

Шины событий для настроек в проекте нет — консьюмеры читают state.userSettings ЛЕНИВО в точке использования, поэтому явного «применения» после Save не нужно:
- hotkeys.js (классический IIFE, не ES-модуль) читает через window.ForgeApp.state.userSettings.cmd_hints_enabled;
- sessions/sessions.js::fetchNextSteps читает state.userSettings.next_step_enabled;
- backend-воркер Echo спрашивает HostApi::next_step_enabled каждый тик.
Переключение подхватывается без перезагрузки страницы и без рестарта сервера.
