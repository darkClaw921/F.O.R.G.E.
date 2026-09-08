//! ДЕСЯТЬ ЛОВУШЕК ФОРМАТА ПОРТАЛА — по тесту на каждую, на ЖИВЫХ ответах.
//!
//! # Почему это отдельный файл, а не разрозненные юнит-тесты
//!
//! Каждая строка из §5 плана — это **падение разбора в проде**: не «поле
//! неудобное», а «панель показывает пустой список, потому что один документ в
//! истории оказался не картинкой». Такие вещи не ловятся типами: JSON приходит
//! из PHP, где `[]` и `{}` — одно и то же значение, а нотация имён зависит от
//! того, какая команда портала писала метод.
//!
//! Поэтому ловушки собраны в одном месте, названы по существу и проверяются на
//! ФИКСТУРАХ ЖИВЫХ ОТВЕТОВ тестового портала (`tests/fixtures/`), снятых
//! 2026-08-05. Разбор идёт ЦЕЛИКОМ, а не по кускам: половина ловушек и состоит
//! в том, что отдельное поле выглядит безобидно, а вместе с соседним не
//! разбирается.
//!
//! | # | ловушка | тест |
//! |---|---|---|
//! | 1 | PHP-массивы: `[]` пусто / `{}` заполнено | [`php_arrays_are_a_list_when_empty_and_a_map_when_filled`] |
//! | 2 | полиморфный `recent[].id` | [`the_recent_id_is_a_string_for_groups_and_a_number_for_dialogs`] |
//! | 3 | `image`: `false` ИЛИ объект | [`the_file_image_field_is_false_or_an_object`] |
//! | 4 | `birthday`: `""` ИЛИ `false` | [`the_wobbly_user_fields_arrive_in_both_forms`] |
//! | 5 | camelCase событий против snake_case методов | [`event_payloads_are_camel_case_while_methods_are_snake_case`] |
//! | 6 | `role` в разном регистре | [`the_role_arrives_in_both_letter_cases`] |
//! | 7 | `total: -1` у `im.recent.list` | [`the_recent_list_total_is_minus_one_and_must_not_be_used`] |
//! | 8 | `LIMIT` не гарантирует ровно N | [`a_limit_does_not_guarantee_exactly_n_rows`] |
//! | 9 | `im.recent.list` не отдаёт чаты задач | [`the_recent_list_omits_task_chats_so_the_list_needs_both_sources`] |
//! | 10 | обрезка 20 006 символов и `URL_PREVIEW` | [`a_long_message_is_silently_truncated_and_url_preview_rewrites_the_text`] |
//! | 11 | `params.LIKE`: строки ИЛИ числа | [`the_like_list_arrives_as_strings_and_as_numbers`] |
//!
//! Одиннадцатая ловушка добавлена ПОСЛЕ живой проверки Фазы 5: разведка сняла
//! `{"LIKE": ["1"]}` (строки), а ответ портала сразу после `im.message.like`
//! содержал `{"LIKE": [1]}` (числа). Список §5 плана оказался неполон — ровно
//! ради этого файл и существует.

use forge_bitrix::api::parse_envelope;
use forge_bitrix::api::schema::{
    CountersRaw, FileRaw, PhpMap, RecentItemRaw, RecentListRaw, StrOrNum, UserMapRaw, UserRaw,
};
use forge_bitrix::bbcode;
use forge_bitrix::dto::{self, NoNames};
use serde_json::Value;

const RECENT_GET: &str = include_str!("fixtures/recent_get.json");
const RECENT_LIST: &str = include_str!("fixtures/recent_list.json");
const MSGS_CHAT27: &str = include_str!("fixtures/msgs_chat27.json");
const MSGS_CHAT3: &str = include_str!("fixtures/msgs_chat3.json");
const EV: &str = include_str!("fixtures/ev.json");
const BATCH1: &str = include_str!("fixtures/batch1.json");

/// Строки списка из живого `im.recent.get` — разобранные ЦЕЛИКОМ.
fn recent_items() -> Vec<RecentItemRaw> {
    let v: Value = serde_json::from_str(RECENT_GET).unwrap();
    serde_json::from_value(v["result"].clone()).expect("живой im.recent.get не разобрался")
}

/// Страница живого `im.recent.list` — разобранная ЦЕЛИКОМ.
fn recent_page() -> RecentListRaw {
    let v: Value = serde_json::from_str(RECENT_LIST).unwrap();
    serde_json::from_value(v["result"].clone()).expect("живой im.recent.list не разобрался")
}

// ===========================================================================
// 1. PHP-массивы: `[]` когда пусто, `{}` когда заполнено.
// ===========================================================================

/// Одно и то же поле приезжает то списком, то картой — потому что в PHP пустой
/// ассоциативный массив неотличим от пустого списка.
///
/// Цена ошибки: строгий `HashMap` падает ровно в САМОМ ЧАСТОМ случае — когда
/// непрочитанного нет.
#[test]
fn php_arrays_are_a_list_when_empty_and_a_map_when_filled() {
    // Живой `im.counters.get`: DIALOG заполнен картой, CHAT пуст массивом.
    let counters: CountersRaw = serde_json::from_str(
        r#"{"TYPE":{"ALL":9,"NOTIFY":5,"CHAT":0,"DIALOG":4,"MESSENGER":4,"TASKS_TASK":0},
            "DIALOG":{"19":1,"17":2,"3":1},"CHAT":[],"CHAT_UNREAD":[],"TASKS_TASK":[]}"#,
    )
    .unwrap();
    assert_eq!(counters.dialog.len(), 3);
    assert_eq!(counters.dialog.into_map().get("17"), Some(&2));
    assert!(counters.chat.is_empty(), "пустой CHAT приехал массивом");

    // Тот же ответ после `im.dialog.read.all`: заполненное поле стало пустым
    // МАССИВОМ, а не пустым объектом.
    let read: CountersRaw = serde_json::from_str(
        r#"{"TYPE":{"ALL":0,"NOTIFY":0,"CHAT":0,"DIALOG":0},"DIALOG":[],"CHAT":[]}"#,
    )
    .unwrap();
    assert!(read.dialog.is_empty());

    // Живой `batch`: `result_error` пуст массивом, `result_total` — объект.
    let batch: Value = serde_json::from_str(BATCH1).unwrap();
    assert!(batch["result"]["result_error"].is_array(), "форма фикстуры изменилась");
    let errors: PhpMap<Value> =
        serde_json::from_value(batch["result"]["result_error"].clone()).unwrap();
    assert!(errors.is_empty());

    // И ответ `im.user.list.get` — карта либо `[]`.
    let filled: UserMapRaw = serde_json::from_value(batch["result"]["result"]["userlist"].clone())
        .expect("карта пользователей не разобралась");
    assert!(!filled.is_empty(), "в фикстуре батча есть пользователи");
    let empty: UserMapRaw = serde_json::from_str("[]").unwrap();
    assert!(empty.into_map().is_empty());
}

// ===========================================================================
// 2. Полиморфный `recent[].id`.
// ===========================================================================

/// У группового чата идентификатор — строка `"chat7"`, у личного — ЧИСЛО `19`.
///
/// Цена ошибки: `String`-поле роняет разбор всего списка на первом же личном
/// диалоге, то есть панель пуста у любого, у кого есть переписка с человеком.
#[test]
fn the_recent_id_is_a_string_for_groups_and_a_number_for_dialogs() {
    let items = recent_items();
    let group = items
        .iter()
        .find(|i| matches!(i.id, StrOrNum::Str(_)))
        .expect("в фикстуре нет строкового id");
    let personal = items
        .iter()
        .find(|i| matches!(i.id, StrOrNum::Num(_)))
        .expect("в фикстуре нет числового id");

    assert_eq!(group.id.as_string(), "chat7");
    assert_eq!(personal.id.as_string(), "19");

    // Наружу обе формы уходят ОДИНАКОВО — строкой: единая форма избавляет
    // фронт от ветвления в ключах Map и в `data-`атрибутах.
    let dto = dto::dialog_from_raw(personal, 1, &NoNames).unwrap();
    assert_eq!(dto.id, "19");
    assert!(serde_json::to_value(&dto).unwrap()["id"].is_string());

    // И `chat_id` — ДРУГОЕ число: у личного диалога 19 ≠ 21.
    assert_eq!(personal.chat_id, Some(21));
    assert_eq!(dto.chat_id, Some(21));
}

// ===========================================================================
// 3. `image`: `false` ИЛИ объект.
// ===========================================================================

/// У документа поле `image` равно `false`, у картинки — объект с размерами.
///
/// Цена ошибки: `Option<Size>` разбирает картинку и падает на документе, то
/// есть лента ломается ровно там, где кто-то прислал `.txt`.
#[test]
fn the_file_image_field_is_false_or_an_object() {
    // Живой файл из разведки (`im.dialog.messages.get`, files[0]) — документ.
    let document: FileRaw = serde_json::from_str(
        r#"{"id":95,"chatId":27,"date":"2026-08-05T20:30:41+03:00","type":"file",
            "name":"forge_probe.txt","extension":"txt","size":500,"image":false,
            "status":"done","progress":100,"authorId":1,"authorName":"Игорь Герасимов",
            "urlPreview":"","mediaUrl":{"preview":{"250":""}},
            "isTranscribable":false,"isVideoNote":false,"isVoiceNote":false}"#,
    )
    .expect("документ не разобрался");
    assert_eq!(document.id, 95);
    assert_eq!(document.size, 500);
    assert_eq!(document.image.size(), None, "у документа размеров нет");

    // Та же структура, но картинка.
    let image: FileRaw = serde_json::from_str(
        r#"{"id":96,"chatId":27,"type":"image","name":"shot.png","extension":"png",
            "size":10240,"image":{"height":61,"width":72},"status":"done","authorId":1}"#,
    )
    .expect("картинка не разобралась");
    assert_eq!(image.image.size(), Some((61, 72)));
    assert_eq!(image.kind.as_deref(), Some("image"));

    // И пустой файл-объект тоже не должен ронять разбор.
    let bare: FileRaw = serde_json::from_str("{}").unwrap();
    assert_eq!(bare.image.size(), None);
}

// ===========================================================================
// 4. `birthday`: `""` ИЛИ `false` (и два его соседа).
// ===========================================================================

/// Три поля пользователя приезжают разными типами в разных методах: `birthday`
/// (`""` / `false`), `last_activity_date` (datetime / `false`),
/// `work_position` (`null` / строка).
///
/// Цена ошибки: разбор участников чата падает на боте — у него `false` там, где
/// у человека строка.
#[test]
fn the_wobbly_user_fields_arrive_in_both_forms() {
    let items = recent_items();

    // Человек: birthday "", last_activity_date — datetime, work_position null.
    let human = items
        .iter()
        .find(|i| i.user.id.as_i64() == Some(19))
        .unwrap();
    assert_eq!(human.user.birthday.as_opt(), None, "\"\" — это «нет значения»");
    assert!(human.user.last_activity_date.as_opt().is_some());
    assert_eq!(human.user.work_position, None);

    // Бот: last_activity_date — false, last_name — null, work_position — строка.
    let bot = items
        .iter()
        .find(|i| i.user.id.as_i64() == Some(17))
        .unwrap();
    assert_eq!(bot.user.last_activity_date.as_opt(), None, "false — тоже «нет»");
    assert_eq!(bot.user.last_name, None);
    assert_eq!(
        bot.user.work_position.as_deref(),
        Some("Помогаем бизнесу работать")
    );
    assert!(bot.user.bot);

    // Форма `false` у самого дня рождения (`im.search.user.list`).
    let searched: UserRaw =
        serde_json::from_str(r#"{"id":15,"name":"Максим","birthday":false}"#).unwrap();
    assert_eq!(searched.birthday.as_opt(), None);
}

// ===========================================================================
// 5. camelCase событий против snake_case методов.
// ===========================================================================

/// Полезная нагрузка `im.v2.Event.get` — camelCase, ответы методов `im.*` —
/// snake_case. Это один и тот же портал и один и тот же пользователь.
///
/// Цена ошибки: поглощение имён из событий (Ф7) молча даёт пользователей без
/// имени, и лента подписывается числами — при этом НИ ОДНОГО сообщения об
/// ошибке нигде не появляется.
#[test]
fn event_payloads_are_camel_case_while_methods_are_snake_case() {
    let ev: Value = serde_json::from_str(EV).unwrap();
    let events = ev["result"]["events"].as_array().unwrap();
    assert!(!events.is_empty(), "фикстура событий пуста");

    let add = events
        .iter()
        .find(|e| e["type"] == "ONIMV2MESSAGEADD")
        .expect("в фикстуре нет ONIMV2MESSAGEADD");

    // Нотация в событии — camelCase.
    let user_json = &add["data"]["user"];
    assert!(user_json["firstName"].is_string(), "фикстура изменилась");
    assert!(user_json.get("first_name").is_none());

    // …и наш `UserRaw` обязан её понимать: иначе имена из событий пропадут.
    let user: UserRaw = serde_json::from_value(user_json.clone()).expect("camelCase не разобрался");
    assert_eq!(user.first_name.as_deref(), Some("Игорь"));
    assert_eq!(user.last_name.as_deref(), Some("Герасимов"));
    assert_eq!(user.last_activity_date.as_opt(), None, "в событии — false");
    assert_eq!(user.work_position, None);

    // Snake_case из методов продолжает работать тем же типом.
    let from_method: UserRaw = serde_json::from_value(
        serde_json::from_str::<Value>(RECENT_GET).unwrap()["result"][0]["user"].clone(),
    )
    .unwrap();
    assert_eq!(from_method.first_name.as_deref(), Some("Игорь"));

    // В самом сообщении портал ДУБЛИРУЕТ обе нотации — на это опирается Ф5.
    let msg = &add["data"]["message"];
    assert_eq!(msg["chatId"], msg["chat_id"]);
    assert_eq!(msg["authorId"], msg["author_id"]);

    // А у чата в событии нотация тоже своя: `entityType` вместо `entity_type`.
    assert!(add["data"]["chat"]["entityType"].is_string());
}

// ===========================================================================
// 6. `role` в разном регистре.
// ===========================================================================

/// `im.recent.get` отдаёт `"OWNER"`, `im.dialog.get` — `"owner"`.
///
/// Цена ошибки: сравнение `role == "owner"` во фронте работает или нет в
/// зависимости от того, каким методом приехала строка, — то есть кнопки
/// управления чатом появляются через раз.
#[test]
fn the_role_arrives_in_both_letter_cases() {
    let items = recent_items();
    let from_recent = items
        .iter()
        .find(|i| i.chat.role.is_some())
        .expect("в фикстуре нет роли");
    assert_eq!(from_recent.chat.role.as_deref(), Some("OWNER"));

    // Тот же чат глазами `im.dialog.get` (живой ответ, фикстура batch1).
    let batch: Value = serde_json::from_str(BATCH1).unwrap();
    assert_eq!(batch["result"]["result"]["chatget"]["role"], "owner");

    // На выходе — одна форма.
    let dto = dto::dialog_from_raw(from_recent, 1, &NoNames).unwrap();
    assert_eq!(dto.role.as_deref(), Some("owner"));
    assert_eq!(dto::normalize_role(Some("OWNER")), dto::normalize_role(Some("owner")));
}

// ===========================================================================
// 7. `total: -1` у `im.recent.list`.
// ===========================================================================

/// Корневой `total` живьём равен `-1` ВСЕГДА.
///
/// Цена ошибки: `has_more = offset + len < total` с `total = -1` даёт `false` с
/// первой же страницы, то есть список молча обрывается на первых сорока
/// строках и никогда не догружается.
#[test]
fn the_recent_list_total_is_minus_one_and_must_not_be_used() {
    let envelope = parse_envelope(200, RECENT_LIST.as_bytes()).expect("конверт не разобрался");
    assert_eq!(envelope.total, Some(-1), "фикстура изменилась");

    let page = recent_page();
    assert!(page.has_more, "признак продолжения — только hasMore");

    // Арифметика по `total` дала бы «страниц больше нет» на первой же
    // странице: `offset + len < total` при `total = -1` — это `4 < -1`.
    let offset: i64 = 0;
    let naive_has_more = offset + (page.items.len() as i64) < envelope.total.unwrap();
    assert!(!naive_has_more, "именно так выглядит ловушка");
    assert_ne!(naive_has_more, page.has_more);
}

// ===========================================================================
// 8. `LIMIT` не гарантирует ровно N записей.
// ===========================================================================

/// Живой замер: `LIMIT=5, OFFSET=0` вернул ЧЕТЫРЕ строки и `hasMore: true` —
/// портал фильтрует уже ПОСЛЕ выборки.
///
/// Цена ошибки: пагинация вида «пришло меньше запрошенного ⇒ это конец»
/// теряет весь хвост списка.
#[test]
fn a_limit_does_not_guarantee_exactly_n_rows() {
    let page = recent_page();
    assert_eq!(page.items.len(), 4, "фикстура снята с LIMIT=5");
    assert!(page.has_more, "…и портал говорит, что это не конец");

    // Признак конца — ТОЛЬКО `hasMore`, а смещение двигается на реально
    // пришедшее число (иначе следующая страница перепрыгнет строки).
    let next_offset = page.items.len() as i64;
    assert_eq!(next_offset, 4, "не 5, хотя просили 5");
    let envelope = parse_envelope(200, RECENT_LIST.as_bytes()).unwrap();
    assert_eq!(envelope.next, Some(4), "портал сам подсказывает смещение");
}

// ===========================================================================
// 9. `im.recent.list` не отдаёт чаты задач.
// ===========================================================================

/// Полный список собирается из ДВУХ источников (Р3): `im.recent.get` знает про
/// чаты задач, `im.recent.list` умеет страницы.
///
/// Цена ошибки: директория `~/tasks` пуста, а чаты задач не видны нигде — при
/// том что оба метода отвечают успехом.
#[test]
fn the_recent_list_omits_task_chats_so_the_list_needs_both_sources() {
    let from_get = recent_items();
    let page = recent_page();

    let tasks_in_get = from_get
        .iter()
        .filter(|i| i.chat.entity_type.as_deref() == Some("TASKS_TASK"))
        .count();
    let tasks_in_list = page
        .items
        .iter()
        .filter(|i| i.chat.entity_type.as_deref() == Some("TASKS_TASK"))
        .count();

    assert!(tasks_in_get >= 2, "im.recent.get обязан отдавать чаты задач");
    assert_eq!(tasks_in_list, 0, "а im.recent.list их не отдаёт вовсе");

    // Склейка двух источников не теряет задачи и не плодит дублей.
    let to_dto = |items: &[RecentItemRaw]| {
        items
            .iter()
            .filter_map(|i| dto::dialog_from_raw(i, 1, &NoNames))
            .collect::<Vec<_>>()
    };
    let merged = dto::merge_dialogs(to_dto(&from_get), to_dto(&page.items));
    assert_eq!(merged.len(), from_get.len(), "склейка дала дубли");
    assert_eq!(
        merged.iter().filter(|d| d.kind == dto::kind::TASK).count(),
        tasks_in_get
    );

    let ids: std::collections::HashSet<&str> = merged.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids.len(), merged.len(), "идентификаторы не уникальны");
}

// ===========================================================================
// 10. Обрезка 20 006 символов и `URL_PREVIEW`.
// ===========================================================================

/// Две ловушки одного поля `text`.
///
/// **Обрезка.** Сообщение длиннее 20 000 символов портал принимает, отвечает
/// успехом и МОЛЧА режет (живой замер: 25 000 → 20 006 символов с хвостом
/// `(...)`). Поэтому длину проверяет плагин (Р12): «успешно отправленный»
/// обрубок хуже честного отказа.
///
/// **`URL_PREVIEW`.** Значение по умолчанию `"Y"` заставляет портал ПЕРЕПИСАТЬ
/// текст: голая ссылка возвращается уже обёрнутой в `[URL]…[/URL]`. Отправленное
/// и прочитанное не совпадают побайтно — на этом ломается наивное сравнение
/// «моё оптимистичное сообщение == пришедшее».
#[test]
fn a_long_message_is_silently_truncated_and_url_preview_rewrites_the_text() {
    // Потолок объявлен в плагине, а не подразумевается.
    assert_eq!(dto::MAX_MESSAGE_CHARS, 20_000);

    // Обрубок портала разбирается и не роняет ни парсер, ни превью.
    let truncated = format!("{}xxxx (...)", "я".repeat(19_996));
    assert_eq!(truncated.chars().count(), 20_006);
    let plain = bbcode::to_plain(&truncated);
    assert_eq!(plain.chars().count(), 20_006);
    let preview = dto::truncate_chars(&plain, dto::PREVIEW_CHARS);
    assert_eq!(preview.chars().count(), dto::PREVIEW_CHARS);

    // `URL_PREVIEW:"Y"`: отправляли голую ссылку — портал вернул обёрнутую.
    let sent = "PROBE plain [B]bold[/B] https://example.com";
    let stored = "PROBE plain [B]bold[/B] [URL]https://example.com[/URL]";
    assert_ne!(sent, stored, "портал переписал текст");
    // Пользователь при этом обязан увидеть ровно то, что писал.
    assert_eq!(bbcode::to_plain(stored), bbcode::to_plain(sent));
    let segments = bbcode::to_segments(stored);
    assert!(segments.iter().any(|s| s.kind == bbcode::kind::URL));
}

// ===========================================================================
// Полный разбор каждой фикстуры целиком.
// ===========================================================================

/// КАЖДЫЙ живой ответ разбирается ПОЛНОСТЬЮ, а не по интересным полям.
///
/// Половина ловушек в том и состоит, что поле по отдельности выглядит
/// безобидно: `"user":{"id":0}` разбирается прекрасно, пока рядом не окажется
/// `"options":[]` вместо объекта.
#[test]
fn every_live_fixture_deserializes_whole() {
    // im.recent.get — плоский массив строк.
    assert_eq!(recent_items().len(), 6);

    // im.recent.list — объект со страницей.
    let page = recent_page();
    assert_eq!(page.items.len(), 4);
    assert!(page.has_more_pages);

    // Конверты обоих ответов разбираются целиком, включая `time`.
    for (name, raw) in [
        ("recent_get", RECENT_GET),
        ("recent_list", RECENT_LIST),
        ("msgs_chat27", MSGS_CHAT27),
        ("msgs_chat3", MSGS_CHAT3),
        ("ev", EV),
        ("batch1", BATCH1),
    ] {
        let envelope = parse_envelope(200, raw.as_bytes())
            .unwrap_or_else(|e| panic!("{name}: конверт не разобрался: {e}"));
        assert!(
            envelope.time.is_some(),
            "{name}: живой ответ обязан нести блок time"
        );
    }

    // Сообщения ленты (Ф5) — `params` приезжает то `[]`, то объектом; здесь
    // проверяется только то, что обе формы разбираются как значение.
    let msgs: Value = serde_json::from_str(MSGS_CHAT27).unwrap();
    let messages = msgs["result"]["messages"].as_array().unwrap();
    assert!(messages.iter().any(|m| m["params"].is_array()));
    assert!(messages.iter().any(|m| m["params"].is_object()));

    // Системные сообщения (`author_id == 0`) с BB-кодами читаются.
    let system: Value = serde_json::from_str(MSGS_CHAT3).unwrap();
    let first = &system["result"]["messages"][0];
    assert_eq!(first["author_id"], 0);
    assert_eq!(
        bbcode::to_plain(first["text"].as_str().unwrap()),
        "Игорь Герасимов добавил права администратора для Максим Крылов."
    );
}

// ===========================================================================
// 11. ОДИННАДЦАТАЯ ЛОВУШКА (найдена в Ф5): `params.LIKE` приезжает то
//     СТРОКАМИ, то ЧИСЛАМИ.
// ===========================================================================

/// Разведка сняла `{"LIKE": ["1"]}` (строки), живая проверка сразу после
/// `im.message.like` — `{"LIKE": [1]}` (числа). Одно поле, один портал, один
/// метод чтения.
///
/// Цена ошибки не «неудобно»: строгий `Vec<String>` роняет разбор ВСЕЙ
/// страницы истории на одном лайкнутом сообщении, и пользователь видит пустой
/// диалог.
#[test]
fn the_like_list_arrives_as_strings_and_as_numbers() {
    use forge_bitrix::attach::likes_from_params;

    // Живая страница: лайк снят после `im.message.like` — числа.
    let live: Value = serde_json::from_str(MSGS_CHAT27).unwrap();
    let liked = live["result"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["params"].get("LIKE").is_some())
        .expect("в фикстуре пропал лайк");
    assert!(
        liked["params"]["LIKE"][0].is_number(),
        "фикстура изменилась: живьём LIKE приходит числами"
    );

    // Обе формы дают ОДИН результат — строковые идентификаторы.
    assert_eq!(likes_from_params(&liked["params"]), vec!["1".to_string()]);
    assert_eq!(
        likes_from_params(&serde_json::json!({"LIKE": ["1"]})),
        vec!["1".to_string()],
        "форма из разведки разбирается так же"
    );
}

/// ЖИВАЯ СТРАНИЦА ИСТОРИИ разбирается ЦЕЛИКОМ — со всеми ловушками разом.
///
/// Здесь их пять в одном ответе: `params` двух форм, `files[]` с `image:
/// false`, `mediaUrl.preview` с пустым значением, `users[]` списком (а не
/// картой) и `LIKE` числами.
#[test]
fn the_live_history_page_deserialises_completely() {
    use forge_bitrix::api::schema::MessagesRaw;

    for (name, raw) in [("msgs_chat27", MSGS_CHAT27), ("msgs_chat3", MSGS_CHAT3)] {
        let v: Value = serde_json::from_str(raw).unwrap();
        let page: MessagesRaw = serde_json::from_value(v["result"].clone())
            .unwrap_or_else(|e| panic!("{name}: живая страница не разобралась: {e}"));
        assert!(!page.messages.is_empty(), "{name}: сообщения потерялись");
        assert!(page.chat_id > 0, "{name}: chat_id обязан приехать");

        // `params` обеих форм превратились в значение, а не в ошибку.
        assert!(page.messages.iter().any(|m| m.params.is_array()));
    }

    let v: Value = serde_json::from_str(MSGS_CHAT27).unwrap();
    let page: MessagesRaw = serde_json::from_value(v["result"].clone()).unwrap();
    assert!(page.messages.iter().any(|m| m.params.is_object()));

    let files = page.files.into_values();
    assert_eq!(files.len(), 1, "файл страницы потерялся");
    assert_eq!(files[0].id, 95);
    assert!(files[0].image.size().is_none(), "документ — не картинка");
    assert!(
        !files[0].media_url.has_preview(),
        "пустой адрес превью — это «превью нет»"
    );

    // `users` живьём приезжает СПИСКОМ, хотя соседние методы отдают карту.
    let users = page.users.into_values();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].id.as_i64(), Some(1));
}

// ---------------------------------------------------------------------------
// ЛОВУШКА 12: явный `null` (и `false`) на месте вложенной СТРУКТУРЫ.
// ---------------------------------------------------------------------------

/// ЯВНЫЙ `null` НА МЕСТЕ СТРУКТУРЫ ОБНУЛЯЛ ВЕСЬ СПИСОК ДИАЛОГОВ.
///
/// Найдено живой проверкой Ф9 на двух стендах: один чат с `entity_link: null`
/// давал панель без ЕДИНОГО диалога — и ни строчки в логе.
///
/// Механика в трёх шагах, и каждый по отдельности выглядит безобидно:
///
/// 1. `#[serde(default)]` покрывает только ОТСУТСТВУЮЩИЙ ключ. Явный `null` на
///    месте структуры — это `invalid type: null`, то есть ошибка разбора ВСЕГО
///    документа, а не одного поля. Пустой PHP-массив `[]` при этом проходит
///    (serde принимает seq-форму структуры), и именно поэтому ловушка не
///    ловилась глазами: `[]` и `null` в ответах портала стоят рядом.
/// 2. Ошибка глохнет в `methods.rs`: `from_value::<Vec<RecentItemRaw>>(…).ok()
///    .unwrap_or_default()` превращает её в пустой вектор без лога.
/// 3. Воркер синхронизации пишет строку только при непустом результате, так
///    что нулевой проход не оставляет следа вовсе.
///
/// Итог для пользователя: `GET /api/bitrix/dialogs` отвечает `200
/// {"dialogs":[]}`, панель пуста, в логе тишина. ОДИН чат роняет ВЕСЬ список —
/// не себя, а список.
///
/// Тест проверяет обе формы «значения нет» (`null` и `false`) на всех
/// структурных полях строки списка и требует, чтобы соседняя строка выжила.
#[test]
fn a_null_in_place_of_a_nested_object_must_not_wipe_the_whole_list() {
    let healthy = serde_json::json!({
        "id": "chat7",
        "chat_id": 7,
        "title": "Живой чат",
        "chat": { "id": 7, "entity_type": "", "entity_link": {} },
        "message": { "id": 1, "text": "привет", "author_id": 1 }
    });

    // Все структурные поля, у которых `#[serde(default)]` НЕ спасает от `null`.
    for field in [
        "chat", "message", "avatar", "user",
    ] {
        for empty in [serde_json::Value::Null, serde_json::Value::Bool(false)] {
            let mut broken = healthy.clone();
            broken[field] = empty.clone();
            let page = serde_json::json!([broken, healthy.clone()]);

            let rows: Vec<RecentItemRaw> = serde_json::from_value(page).unwrap_or_else(|e| {
                panic!("recent[].{field} = {empty}: разбор списка упал целиком: {e}")
            });
            assert_eq!(
                rows.len(),
                2,
                "recent[].{field} = {empty}: строки потерялись"
            );
            assert_eq!(
                rows[1].chat.id, Some(7),
                "recent[].{field} = {empty}: соседняя ЗДОРОВАЯ строка пострадала"
            );
        }
    }

    // То же внутри `chat` — там ловушка и была найдена живьём.
    for field in ["entity_link", "restrictions", "mute_list", "manager_list"] {
        for empty in [serde_json::Value::Null, serde_json::Value::Bool(false)] {
            let mut broken = healthy.clone();
            broken["chat"][field] = empty.clone();
            let page = serde_json::json!([broken, healthy.clone()]);

            let rows: Vec<RecentItemRaw> = serde_json::from_value(page).unwrap_or_else(|e| {
                panic!("chat.{field} = {empty}: разбор списка упал целиком: {e}")
            });
            assert_eq!(rows.len(), 2, "chat.{field} = {empty}: строки потерялись");
        }
    }

    // `send` по умолчанию РАЗРЕШЁН, и обнулённые ограничения не имеют права
    // молча запереть поле ввода.
    let mut broken = healthy.clone();
    broken["chat"]["restrictions"] = serde_json::Value::Null;
    let rows: Vec<RecentItemRaw> = serde_json::from_value(serde_json::json!([broken])).unwrap();
    assert!(
        rows[0].chat.restrictions.send,
        "обнулённые ограничения обязаны читаться как «писать можно»"
    );
}
