# bitrix-keys

# bitrix-keys — клавиатура панели «Битрикс24» (Ф11)

Три файла, три уровня: js/messenger/hotkeys-core.js (движок комбинаций, общий на все панели), js/bitrix/keys.js (реестр действий, чистый) и js/bitrix/keynav.js (DOM, фокус, курсор).

## По event.code, а не по event.key

В мессенджере печатают по-русски. При русской раскладке event.key физической клавиши J вернёт 'о', и хоткей j перестал бы работать ровно у целевой аудитории. Канонический вид комбинации — [Ctrl+][Alt+][Shift+][Meta+]<клавиша>, порядок модификаторов фиксирован, чтобы строка годилась ключом Map.

## Реестр действий

B24_ACTIONS — 23 действия, 5 групп: Навигация (focus.dialogs d, focus.log m, focus.input i, nav.next j, nav.prev k, nav.activate Enter, dialog.unread u, dir.next ], dir.prev [), Сообщение (msg.reply r, msg.edit e, msg.like l, msg.copy y, msg.buttons b, msg.attachments v), Задача (task.card c, task.actions x), Лента (log.bottom Shift+g, log.older g), Панель (ui.attach a, ui.theme t, ui.help Shift+/, ui.escape Escape — fixed).

Совпадение дефолтов с Telegram и VK намеренное: три панели одного приложения не должны вести себя как три разные программы. Собственные действия — msg.edit (портал разрешает правку своего сообщения), msg.like (единственная реакция вебхука) и группа «Задача» (карточка над лентой с матрицей действий портала). Убран msg.quote: отдельного узла цитаты в ленте нет.

## Владение клавиатурой

b24OwnsKeyboard() = пейн #bitrix видим И #b24-login скрыт И bitrix_hotkeys_enabled !== false И (фокус нигде ИЛИ фокус внутри #bitrix). Кликнув в сайдбар сессий, пользователь ВОЗВРАЩАЕТ СЕБЕ vim — это правило, а не побочный эффект. Одновременно владеть панели не могут: switchTab держит непогашенным ровно один пейн.

## preventDefault только при выполненном действии

runAction(id) возвращает true, лишь когда действие реально что-то сделало; только тогда событие гасится. Ложное true съело бы клавишу у соседей, ничего не выполнив, — дефект, невидимый глазами. Отдельный случай: если действие невозможно (у чужого сообщения нет кнопки правки, у сообщения нет вложений), панель показывает короткое уведомление и ВСЁ РАВНО гасит клавишу — иначе следом сработал бы vim и увёл фокус, спрятав только что показанный текст.

## Настройки

bitrix_hotkeys_enabled — дефолт ВКЛЮЧЕНО, читается !== false. bitrix_hotkeys — карта переопределений id -> комбинация, полная замена при сохранении. Кэш разбора ключуется СОДЕРЖИМЫМ карты, поэтому сохранённые настройки действуют сразу, без перезагрузки. Форма — js/settings/hotkeys-tab.js::B24_HOTKEYS_CONFIG.

Свод гейтов, который легко перепутать: cmd_hints_enabled === true (дефолт ВЫКЛ), telegram_hotkeys_enabled !== false, vk_hotkeys_enabled !== false, bitrix_hotkeys_enabled !== false, bitrix_filter_enabled === true (дефолт ВЫКЛ).

## Реестр PANEL_KEY_CONTRACTS -> keyboardOwners

Подробности — на странице keyboard-owners. Коротко: до третьей панели static/hotkeys.js хранил захардкоженный список имён контрактов и обещал реестр «когда появится третья». Битрикс24 — третья: введён массив window.ForgeApp.keyboardOwners, в который панель дописывает себя сама (keynav.js::registerKeyboardOwner). Имена двух прежних панелей остались засеянными в hotkeys.js — их регистрация потребовала бы правки работающего чужого кода.

Тесты: tmux-web/tests/frontend/bitrix_keys.test.mjs (96 ассертов), hotkeys_panel_owner.test.mjs (реестр), settings_hotkeys_form.test.mjs (три конфига адресуют разные поля).

