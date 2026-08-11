ВЛОЖЕНИЯ, КАРТОЧКИ И КЛАВИАТУРЫ (plugins/bitrix/src/attach.rs, Ф5, клавиатуры расширены в forge-zw3d).

ТРИ НЕЗАВИСИМЫЕ СУЩНОСТИ В ОДНОМ ПОЛЕ params:
- FILE_ID: [95] — приложенные файлы; сами дескрипторы лежат в КОРНЕ ответа im.dialog.messages.get (files[]), а не в сообщении → MediaDto;
- ATTACH: [{BLOCKS:[...]}] — карточка бота/робота → CardDto;
- KEYBOARD: [{...}] — inline-кнопки → ReplyMarkupDto.

СВЯЗКА ФАЙЛОВ. index_files(files) строит карту id→дескриптор ОДИН раз на страницу; media_from_params ходит по params.FILE_ID и берёт ПОРЯДКОВЫЙ НОМЕР как MediaDto::index. Индекс не декоративен: он входит в ключ кэша и в путь прокси /api/bitrix/media/file/:dialog/:mid/:index (Ф8) — без него два файла одного сообщения делили бы один ключ и второй затирал бы первый. Идентификатор без дескриптора НЕ выбрасывается: получается заглушка (has_full=false), потому что скачивание всё равно адресуется file_id, а молча потерянное вложение выглядит как «ничего не присылали».

MediaDto: index, file_id, kind (image|video|video_note|audio|voice|file), name, extension, size, width/height, has_thumb, has_full, player_url (ВСЕГДА None), waveform (ВСЕГДА пусто). Вид определяется в порядке: isVoiceNote → voice, isVideoNote → video_note, затем type; незнакомый type — это file, а не ошибка. has_thumb = НЕПУСТОЙ mediaUrl.preview['250'] (ключ портал присылает всегда, значение у документа пустое). player_url и waveform объявлены ради ИДЕНТИЧНОСТИ ФОРМЫ с VK/Telegram — у портала их нет.

ГЛАВНЫЙ ИНВАРИАНТ: внешних адресов файла в DTO НЕТ НИКОГДА. urlDownload, urlShow и viewerAttrs.unifiedLink портал присылает, но они не работают без сессионной куки (живая проверка: первые два отдают HTML страницы логина, третий — 403). Единственный рабочий путь — im.v2.File.download → downloadUrl, а тот содержит код вебхука. Поэтому этих полей нет ни в MediaDto, ни в schema.rs::FileRaw: не объявить проще, чем не отдать. Тест-страж no_portal_download_url_survives_the_projection читает живую фикстуру, которая эти поля СОДЕРЖИТ.

КАРТОЧКИ ATTACH. cards_from_params → CardDto {id, color, description, blocks}. Блок бывает трёх видов (block_kind): message (текст с BB-кодами → text + segments), link (NAME/LINK/DESC), grid (NAME/VALUE/DISPLAY/COLOR). Незнакомый вид блока пропускается МОЛЧА — портал добавляет виды между релизами. Карточка без единого знакомого блока и без описания не рисуется вовсе. Ссылки карточки (в отличие от кнопок клавиатуры) НЕ кликабельны — рисуются текстом, чисто информационно (attachments.js).

КЛАВИАТУРЫ (ПЕРЕПИСАНО в forge-zw3d). keyboard_from_params → ReplyMarkupDto {kind: 'inline', rows}. Портал отдаёт кнопки ОДНИМ плоским массивом, а раскладку прячет в двух местах: служебный элемент {TYPE:'NEWLINE'} начинает новый ряд, DISPLAY:'BLOCK' занимает ряд целиком, остальное (DISPLAY:'LINE') идёт в текущий ряд. Кнопка без подписи не рисуется — нажимать нечего.

ButtonDto {text, action, value, enabled, disabled_reason, bg_color, text_color, command, command_params, bot_id, lock_after_press}. ШЕСТЬ действий, а не четыре:
- put/send — работают КЛИЕНТСКИ (put подставляет текст в поле ввода, send отправляет тем же путём, что Enter);
- link (НОВОЕ) — открыть URL из поля LINK. Работает КЛИЕНТСКИ (window.open), value = проверенный http_url. САМЫЙ ЧАСТЫЙ вид кнопки у РЕАЛЬНЫХ ботов портала (живой пример — «Получить консультацию» у системного бота «Команда заботы Битрикс24»);
- command (НОВОЕ) — выполнить команду бота (COMMAND+COMMAND_PARAMS из поля button, BOT_ID отдельным полем). Требует СЕТЬ: POST /api/bitrix/messages/:mid/command → im.message.command. В ОТЛИЧИЕ от call/dialog НЕ требует scope imbot — живая проба портала (test-work-2) подтвердила успех обычным входящим вебхуком. Не идемпотентен, в NO_RETRY_METHODS. Живой пример — кнопки оценки 1..10 того же бота (COMMAND: careButton). Кнопка без BOT_ID приезжает выключенной с причиной COMMAND_NEEDS_BOT_ID;
- call/dialog — требуют scope imbot, которого у входящего вебхука нет и быть не может, поэтому приезжают enabled=false с текстом причины в disabled_reason (§6: кнопка, которая молча ничего не делает, хуже отсутствующей).
DISABLED:'Y' от портала — ВТОРАЯ, отдельная причина: «выключил отправитель» лечится не тем же, что «прав не хватает».
lock_after_press = BLOCK:'Y' портала — просьба НАВСЕГДА заблокировать кнопку ПОСЛЕ УСПЕШНОГО command (фронт: класс b24-kb-btn-off вместо временного b24-kb-btn-pending; ошибка кнопку не блокирует — решение остаётся за пользователем).

ЖИВАЯ НАХОДКА №2 (forge-zw3d, botkb.js::buildButton): bg_color/text_color — ПАРА, которую бот подобрал под ЗАЛИВКУ кнопки (портал заполняет ею весь прямоугольник в официальных клиентах), а не только под рамку. До фикса bg_color красил ТОЛЬКО border, и тёмный text_color (например #000 у 'Команда заботы Битрикс24') ложился на прозрачный тёмный фон панели и был НЕВИДИМ — кнопка выглядела пустым прямоугольником, хотя нажималась. Теперь bg_color красит и backgroundColor, и borderColor — контраст, подобранный автором бота, работает как задумано.

САНИТАЙЗЕРЫ ЧУЖОГО ТЕКСТА (первый рубеж; второй — во фронте, botkb.js::safeUrl для link).
- http_url: адрес карточки/LINK-кнопки пропускается только со схемой http/https, регистронезависимо, без управляющих символов, длиной до 2048. javascript:, data:, file:, //host отсекаются.
- hex_color: цвет уезжает в атрибут style, поэтому принимается только #rgb / #rrggbb.
- Все подписи и тексты обрезаются по code point'ам (MAX_LABEL_CHARS, MAX_CARD_TEXT_CHARS), число блоков/строк/кнопок ограничено (MAX_CARDS, MAX_BLOCKS, MAX_ROWS, MAX_BUTTONS, MAX_MEDIA).

ПРОЧИЕ ЧИТАТЕЛИ params: likes_from_params (LIKE строками ИЛИ числами → всегда строки), action_from_params (CODE[0]), is_yes (флаги 'Y': IS_EDITED, SYSTEM, DISABLED, BLOCK).

СВЯЗИ: используется dto::message_from_raw; сырьё — api/schema.rs::{FileRaw, MediaUrlRaw, MessageRaw}; фронтовые потребители — tmux-web/static/js/bitrix/attachments.js и botkb.js; сеть command-кнопки — routes/dialogs.rs::post_command + api/methods.rs::message_command. Подробности бага и живой приёмки — страница bitrix-forge-zw3d-fix.