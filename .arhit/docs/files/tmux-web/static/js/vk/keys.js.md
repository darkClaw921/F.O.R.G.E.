# tmux-web/static/js/vk/keys.js

РЕЕСТР ДЕЙСТВИЙ КЛАВИАТУРЫ ВКЛАДКИ VK (Ф11).

Движка комбинаций здесь НЕТ — он в js/messenger/hotkeys-core.js и общий на все панели-мессенджеры. Здесь только то, что у панели своё: VK_ACTIONS (20 действий), VK_ACTION_GROUPS = ['Навигация','Сообщение','Лента','Панель'] и тонкие обёртки, подставляющие реестр первым аргументом (defaultBindings, sanitizeOverrides, resolveBindings, bindingRows, overridesFromBindings, groupedRows, findAction, actionForEvent); чистые функции движка (keyFromEvent, comboFromEvent, normalizeCombo, formatCombo, isEditableTarget, comboAllowedIn) реэкспортируются как есть.

Модуль чистый: ни DOM, ни сети, ни импортов из соседних модулей вкладки. DOM-часть — в keynav.js.

## Состав дефолтов

Навигация: focus.dialogs d, focus.log m, focus.input i, nav.next j, nav.prev k, nav.activate Enter, dialog.unread u, dir.next ], dir.prev [.
Сообщение: msg.reply r, msg.quote o, msg.copy y, msg.buttons b, msg.attachments v.
Лента: log.bottom Shift+g, log.older g.
Панель: ui.attach a, ui.theme t, ui.help Shift+/, ui.escape Escape (fixed).

Совпадение с телеграмными дефолтами намеренное: пользователь, привыкший к j/k/r/y в Telegram, обязан получить их же в VK, иначе две панели одного приложения ведут себя как две разные программы.

## Три отличия от телеграмного реестра

1. dir.next / dir.prev ходят по ЧЕТЫРЁМ СЕРВЕРНЫМ фильтрам VK (all/unread/important/unanswered), а не по папкам аккаунта: у VK папок нет, а фильтр — параметр messages.getConversations, то есть КАЖДОЕ переключение стоит сетевого запроса. Дебаунс 300 мс и уважение к лимиту 3 rps живут в dialogs.js::selectDir (вторая точка правды о частоте запросов в keynav НЕ заводится); подсказка строки предупреждает об этом.
2. msg.buttons остаётся, но у VK нажимаются только кнопки text и open_link: callback/vkpay/open_app/location требуют group-токена и рисуются ОТКЛЮЧЁННЫМИ (botkb.js). Фокус на такую кнопку встанет, нажатие ничего не сделает — это честнее, чем прятать её.
3. ДОБАВЛЕНО msg.attachments (v): у VK на одном сообщении до десяти вложений (msg.attachments — массив), и 'открыть N-е' без клавиатуры недостижимо. У Telegram такого действия не было и быть не могло — там msg.media ОДИН объект, и nav.activate покрывал весь случай.

## Инварианты, закреплённые тестом vk_keys.test.mjs

id действий уникальны и проходят backend-санитайзер (только [a-z0-9_.-] — user_settings.rs::sanitize_hotkeys режет остальное); дефолтные комбинации не пересекаются (дубль означал бы действие, стартующее БЕЗ клавиши); fixed только у ui.escape (Escape — единственный выход из режима, отдать его под переназначение значит позволить запереть себя внутри).
