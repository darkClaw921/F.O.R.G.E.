# tmux-web/static/js/settings/modal.js

Модалка настроек — диспетчер вкладок. Единственный экспорт: openSettingsModal(initialTab).

## Вкладки

Notifications (дефолтная, глобальный notifier-config), Themes, TODO behavior, Echo, Сводка дня, Интерфейс, и опциональная Remote servers (только isRemoteMode()).

## Реестра табов НЕТ

Всё захардкожено. Чтобы добавить вкладку, нужно править 4+ места:
1. кнопка в HTML-строке card.innerHTML: <button class='modal-tab-btn' data-tab='X' role='tab'>
2. панель: <div class='modal-tab-panel' id='ps-panel-X' data-panel='X' hidden> с внутренним #ps-X-content
3. ветка в showTab(name): if (name === 'X') renderXPanel()
4. whitelist в конце: initialTab === 'X'
плюс селектор , объект состояния xTabState = { loaded: false }, импорт и шапка-комментарий.

## Ленивый рендер

Каждая вкладка рендерится один раз при первом показе (флаг loaded). Паттерн (renderTodoPanel / renderEchoPanel / renderInterfacePanel): показать 'Loading settings…' → если state.userSettings === null, await fetchUserSettings() → передать state.userSettings || {} в фабрику формы. Пустой объект как graceful-fallback: backend down → форма всё равно показывается с дефолтами.

Исключение — Notifications: при ошибке загрузки форма НЕ показывается (иначе Save затрёт реальный конфиг дефолтами), вместо неё .settings-load-error с кнопкой «Повторить», сбрасывающей notifierState.loaded.

Вкладка «Сводка дня» настроек не хранит (только действия), поэтому fetchUserSettings не зовёт; ей передаётся close как onClose.

## Контракт форм

buildXxxForm(settings, onSaved) → DOM-узел, либо renderXxxTab(container, settings, onSaved). onSaved(updated) пишет свежий снапшот в state.userSettings (формально избыточно — updateUserSettings уже это делает).

## Вкладка «Интерфейс»

renderInterfacePanel → buildInterfaceForm из ./interface-tab.js. Тумблеры двух opt-in фич (cmd_hints_enabled, next_step_enabled), обе по умолчанию выключены. Отдельного «применения» настройки не требуется: консьюмеры читают флаги лениво. См. [[interface-settings-toggles]].

## Открытие

- Кнопка ⚙ #project-settings в шапке (core/bootstrap.js вешает openSettingsModal как listener напрямую → initialTab получает объект Event, не проходит whitelist → открывается дефолтная вкладка Notifications; работает по случайности).
- Таб [+] в origin-tabs (только remote-mode): openSettingsModal('remotes').

## Стили

css/settings-modal.css (.modal-card.settings-modal, .modal-tabs, .modal-tab-btn, .modal-tab-panel) + modals.css + notifications.css (фактический дизайн-язык форм: .notify-fieldset / .notify-field / .notify-hint / .notify-error / .notify-actions). Новые вкладки переиспользуют эти классы и правок CSS не требуют.
