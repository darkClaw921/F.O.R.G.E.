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

## Вкладка «VK» (Ф10 плагина VK)

renderVkPanel → buildVkVisibilityForm из ./vk-visibility-tab.js. Ленивый рендер тем же паттерном, что у соседей: при state.userSettings === null сначала fetchUserSettings(), при недоступном бэкенде форма строится с дефолтами (пустой объект). Вкладка ОТДЕЛЬНАЯ, а не общая «Мессенджеры»: объединение потребовало бы переписать работающую панель Telegram — ровно то, чего мы избегаем; слияние станет отдельной задачей на момент появления третьего мессенджера.

Реестра табов в файле нет — каждая вкладка захардкожена в ШЕСТИ местах: кнопка (data-tab), панель (#ps-panel-<name>), селектор контента, объект состояния {loaded}, ветка в showTab, whitelist initialTab. Это норма файла: копия шести строк дешевле абстракции ради восьми вкладок.

Строка табов стала из семи кнопок восьмью (девятью в remote-mode), поэтому .modal-tabs получил flex-wrap: wrap — дешёвое лечение переполнения вместо реструктуризации. Классы vk-visibility-* объявлены в css/settings-modal.css общим правилом с телеграмными tg-visibility-*.

В Ф11 в эту же панель добавится форма хоткеев VK (порядок тот же: сначала видимость, потом клавиши).

Ф11. Добавлена ДЕВЯТАЯ вкладка — «Битрикс24» (data-tab="bitrix"), теми же шестью механическими правками, что и соседние: кнопка, панель #ps-panel-bitrix, реф #ps-bitrix-content, объект ленивого состояния, renderBitrixPanel() и ветка в showTab + whitelist initialTab. Реестра табов по-прежнему нет, и это осознанно: копия шести строк дешевле абстракции ради девяти вкладок.

renderBitrixPanel строит ДВА раздела в том же порядке, что у Telegram и VK: видимость диалогов (buildBitrixVisibilityForm — свой файл, потому что список строится из видов диалога портала) и горячие клавиши (общая buildHotkeysForm с B24_HOTKEYS_CONFIG). Порядок значим: видимость меняет ТО, ЧТО видно на экране, а хоткеи — то, КАК этим управлять.

При недоступном backend в форму уходит пустой объект: форма видимости покажет фильтр выключенным, а форма клавиш — тумблер ВКЛЮЧЁННЫМ. Дефолты этих двух настроек намеренно разные (=== true против !== false), и пустой объект обязан давать именно такую пару.


Ф6 аналитики. Добавлена вкладка «Аналитика» (data-tab="analytics") — теми же механическими правками, но их СЕМЬ, и все обязательны: импорт renderAnalyticsTab из ./analytics-tab.js плюс getAnalyticsSummary/getAnalyticsStatus/rescanAnalytics из ../analytics/api.js; кнопка; панель #ps-panel-analytics; селектор #ps-analytics-content; analyticsTabState; renderAnalyticsPanel() в ветке showTab; whitelist initialTab. Полнота регистрации теперь под стражем — tests/frontend/settings_analytics_tab.test.mjs читает этот файл текстом и проверяет все семь точек (у «Встреч» такой же страж в settings_meet_tab.test.mjs).

Вкладка аналитики, в отличие от соседей, НЕ пользуется state.userSettings и НЕ рисуется условно: индекс встроен в бинарь и есть всегда. Сеть ей передаётся ИНЪЕКЦИЕЙ (образец «Встреч»), а не берётся ею самой — вкладка обязана грузиться в Node без браузера. Список функций обязан быть полным: пропущенное имя вкладка переживает молча, и живьём это выглядит как кнопка пересбора, которая ничего не делает.

Ленивый рендер здесь — не экономия, а условие: первый запрос к /api/analytics/* ЗАПУСКАЕТ индексацию ~/.claude/projects (~2.7 ГБ). Рисуй модалка вкладку при открытии — скан стартовал бы у всякого, кто зашёл поменять тему. Вкладке передаётся close как onClose: кнопка «Открыть на весь экран» закрывает модалку перед показом дашборда, иначе он окажется под оверлеем настроек. См. [[analytics-dashboard]].

## Статистика агентов

Актуальный lifecycle аналитики: импорт agentsApi используется только для передачи getAgentLive:agentsApi.live в renderAnalyticsTab. Возвращённые refs сохранены в analyticsTabState.view. close() вызывает dispose до удаления overlay; showTab при уходе с analytics вызывает dispose и сбрасывает loaded, чтобы возвращение создало живой renderer с единственным таймером. Это устраняет оставшийся после закрытия опрос и не затрагивает остальные вкладки.
