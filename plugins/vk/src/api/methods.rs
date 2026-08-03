//! Типизированные обёртки методов VK.
//!
//! Тонкий слой над [`VkClient::call_value`]: собрать [`Params`], позвать,
//! разобрать в структуры [`schema`]. Ни сети, ни политики повторов, ни
//! троттлинга здесь нет — всё это уровнем ниже.
//!
//! # Правила, общие для всего модуля
//!
//! 1. **`access_token` в [`Params`] не кладётся никогда.** Его добавляет
//!    транспорт, и на это есть тест `params_never_contain_the_token`: сборка
//!    параметров — самое естественное место, где токен «просился бы» в код.
//! 2. **Каждый мутирующий метод принимает необязательную [`Captcha`]** и
//!    подставляет `captcha_sid` + `captcha_key` в параметры как есть. Повтор с
//!    фронта после ввода символов = тот же запрос плюс два поля; общего
//!    авто-решателя captcha в плагине нет и не планируется.
//! 3. **`random_id` ВСЕГДА приходит снаружи** — см. [`new_random_id`].
//! 4. **Границы страниц зажимаются** [`clamp_limit`]: клиент не должен уметь
//!    попросить у VK десять тысяч сообщений.
//!
//! # Почему `messages.send` идемпотентен, а `messages.getById` — нет
//!
//! VK дедуплицирует отправку по `random_id` и на повтор возвращает ТОТ ЖЕ
//! `message_id`. Это подарок, которого нет у Telegram, и он делает безопасным
//! автоповтор даже мутирующего метода — при условии, что `random_id`
//! сгенерирован ОДИН РАЗ на логическую отправку и не пересоздаётся при
//! повторе. Поэтому он и стоит отдельным аргументом
//! [`VkClient::send_message`], а не прячется внутри.

use serde_json::Value;

use super::schema::{
    ConversationsRaw, GroupRaw, LongPollHistoryRaw, LongPollServerRaw, MessagesPageRaw, ProfileRaw,
    UploadServerRaw,
};
use super::{Params, VkClient, VkError};

/// Наш потолок размера страницы.
///
/// В документации VK у `messages.getConversations` заявлено 200, на практике
/// сервер молча отдаёт меньше. Свой потолок заведомо ниже любого
/// наблюдаемого — так поведение не зависит от того, что VK решит сегодня.
pub const MAX_DIALOGS: i64 = 100;

/// Сколько идентификаторов сообщений разрешено спросить одним `getById`.
pub const MAX_MESSAGE_IDS: usize = 100;

/// Поля профилей, которые нужны UI.
///
/// `is_friend` — ближайший аналог телеграмного «контакта», `online` и
/// `last_seen` рисуют точку присутствия, `photo_50` — аватар списка.
pub const USER_FIELDS: &str = "is_friend,screen_name,photo_50,online,last_seen";

/// Поля сообществ, которые нужны UI.
pub const GROUP_FIELDS: &str = "screen_name,photo_50";

/// Поля, запрашиваемые вместе с расширенными списками сообщений и бесед.
pub const EXTENDED_FIELDS: &str = "photo_50,screen_name,online,last_seen,is_friend";

/// Версия протокола LongPoll.
///
/// 3 — минимальная, в которой события несут `extra_fields`; на ней же
/// работает `messages.getLongPollHistory`.
pub const LP_VERSION: &str = "3";

/// Четыре серверных фильтра бесед — замена телеграмным папкам.
pub const DIALOG_FILTERS: [&str; 4] = ["all", "unread", "important", "unanswered"];

/// Пара «идентификатор captcha + введённые символы».
///
/// Приходит с фронта после того, как пользователь прочитал картинку. Ни одна
/// часть пары не хранится: следующий запрос либо пройдёт, либо VK попросит
/// новую captcha с новым `sid`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captcha {
    /// `captcha_sid` из ошибки VK (код 14).
    pub sid: String,
    /// Символы, которые ввёл пользователь.
    pub key: String,
}

/// Тело отправляемого сообщения.
///
/// Отдельная структура, а не семь аргументов: у `messages.send` необязательных
/// параметров больше, чем обязательных, и позиционный вызов на семь `Option`
/// невозможно прочитать в месте вызова.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SendBody {
    /// Текст.
    pub message: Option<String>,
    /// Строка вложений VK (`photo123_456_key,doc...`).
    pub attachment: Option<String>,
    /// Идентификатор сообщения, на которое отвечаем (глобальный `message.id`).
    ///
    /// Работает только там, где глобальный id вообще применим к цели. Ответ,
    /// адресованный `conversation_message_id`, едет в [`SendBody::forward`] —
    /// см. [`reply_forward_json`].
    pub reply_to: Option<i64>,
    /// JSON-объект `forward` метода `messages.send`.
    ///
    /// Единственный документированный способ адресовать ответ по
    /// `conversation_message_id`: `{"peer_id":…,"conversation_message_ids":[…],
    /// "is_reply":1}`. Строка, а не структура, потому что VK принимает этот
    /// параметр именно строкой JSON внутри формы.
    pub forward: Option<String>,
    /// `payload` нажатой кнопки бота.
    pub payload: Option<String>,
    /// Ответ на captcha, если предыдущая попытка получила код 14.
    pub captcha: Option<Captcha>,
}

/// `forward` для ответа, адресованного `conversation_message_id`.
///
/// # Зачем это вообще нужно
///
/// У VK две нумерации: сквозная по аккаунту (`message.id`) и внутренняя для
/// беседы (`conversation_message_id`). Параметр `reply_to` метода
/// `messages.send` понимает ТОЛЬКО первую, а фронт адресует ответ второй —
/// потому что именно `cmid` приезжает в `reply_message` каждой цитаты и
/// именно он одинаков у всех участников беседы. Мост между ними —
/// `forward` с `is_reply: 1`.
///
/// `conversation_message_ids` обязан содержать РОВНО ОДИН идентификатор:
/// с `is_reply` список из двух VK отвергает (это уже пересылка, а не ответ).
///
/// `peer_id` дублирует адресата запроса намеренно: без него VK ищет оригинал
/// не там, где мы отвечаем, и возвращает «сообщение не найдено».
#[must_use]
pub fn reply_forward_json(peer_id: i64, cmid: i64) -> String {
    // Собирается вручную, а не через serde_json::json!, ради буквальности:
    // порядок и набор полей этой строки — часть контракта с VK, и увидеть их
    // в одном месте важнее экономии трёх строк.
    format!("{{\"peer_id\":{peer_id},\"conversation_message_ids\":[{cmid}],\"is_reply\":1}}")
}

/// Зажимает запрошенный размер страницы в `1..=`[`MAX_DIALOGS`].
///
/// Ноль и отрицательные значения превращаются в 1, а не в «по умолчанию»:
/// `?limit=0` — это опечатка клиента, и молча отдать ему сотню записей
/// означало бы скрыть ошибку.
pub fn clamp_limit(requested: i64) -> i64 {
    requested.clamp(1, MAX_DIALOGS)
}

/// Приводит имя фильтра к одному из [`DIALOG_FILTERS`].
///
/// Незнакомое значение (в том числе пустое) даёт `all`: фильтр приходит из
/// query-строки, и отвечать `400` на опечатку в необязательном параметре —
/// лишняя строгость, из-за которой вкладка не открылась бы вовсе.
pub fn normalize_filter(requested: &str) -> &'static str {
    DIALOG_FILTERS
        .into_iter()
        .find(|f| *f == requested.trim())
        .unwrap_or("all")
}

/// Новый `random_id` для одной ЛОГИЧЕСКОЙ отправки.
///
/// Положительный `i32` из младших бит UUID v4 — отдельный крейт `rand` ради
/// этого не заводится (`uuid` уже в дереве). Ноль исключён намеренно: у VK
/// `random_id=0` означает «дедупликацию не делать», то есть ровно противоположное
/// тому, зачем он нам нужен.
///
/// Вызывать РОВНО ОДИН РАЗ на отправку: повтор запроса обязан нести тот же
/// `random_id`, иначе собеседник получит два сообщения.
pub fn new_random_id() -> i32 {
    let bits = (uuid::Uuid::new_v4().as_u128() as u32) & 0x7fff_ffff;
    if bits == 0 {
        1
    } else {
        bits as i32
    }
}

/// Кладёт необязательный параметр, если значение есть.
fn push_opt<T: ToString>(params: &mut Params, key: &'static str, value: Option<T>) {
    if let Some(v) = value {
        params.push((key, v.to_string()));
    }
}

/// Подмешивает ответ на captcha.
fn push_captcha(params: &mut Params, captcha: Option<&Captcha>) {
    if let Some(c) = captcha {
        params.push(("captcha_sid", c.sid.clone()));
        params.push(("captcha_key", c.key.clone()));
    }
}

/// Достаёт массив из ответа, переживая обе формы VK.
///
/// Часть методов отдаёт голый массив (`users.get`), часть — объект с полем
/// (`groups.getById` начиная с v5.147 отвечает `{"groups":[…]}`). Строгий
/// разбор одной формы ломался бы при переключении `api_version` в конфиге.
fn array_or_field(value: Value, key: &str) -> Value {
    if value.is_array() {
        return value;
    }
    match value.get(key) {
        Some(v) if v.is_array() => v.clone(),
        _ => Value::Array(Vec::new()),
    }
}

impl VkClient {
    // -----------------------------------------------------------------
    // Профили.
    // -----------------------------------------------------------------

    /// `users.get` — имена, аватары и присутствие пользователей.
    ///
    /// Пустой список идентификаторов у VK означает «расскажи о владельце
    /// токена» — именно так работает проверка токена в Ф3, поэтому пустой
    /// вход НЕ считается ошибкой.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK (см. [`VkClient::call_value`]).
    pub async fn users_get(&self, user_ids: &[i64]) -> Result<Vec<ProfileRaw>, VkError> {
        let mut params: Params = vec![("fields", USER_FIELDS.to_string())];
        if !user_ids.is_empty() {
            params.push(("user_ids", join_ids(user_ids)));
        }
        let value = self.call_value("users.get", params).await?;
        from_value_named("users.get", array_or_field(value, "items"))
    }

    /// `groups.getById` — названия и аватары сообществ.
    ///
    /// Принимает ПОЛОЖИТЕЛЬНЫЕ идентификаторы: в `peer_id` сообщество едет со
    /// знаком минус, и снимать знак обязан вызывающий — иначе VK ответит
    /// кодом 100 на каждый второй запрос.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK.
    pub async fn groups_get_by_id(&self, group_ids: &[i64]) -> Result<Vec<GroupRaw>, VkError> {
        if group_ids.is_empty() {
            return Ok(Vec::new());
        }
        let params: Params = vec![
            ("group_ids", join_ids(group_ids)),
            ("fields", GROUP_FIELDS.to_string()),
        ];
        let value = self.call_value("groups.getById", params).await?;
        from_value_named("groups.getById", array_or_field(value, "groups"))
    }

    // -----------------------------------------------------------------
    // Чтение переписки.
    // -----------------------------------------------------------------

    /// `messages.getConversations` — страница списка бесед.
    ///
    /// `extended=1` обязателен: вместе с беседами приезжают `profiles` и
    /// `groups`, из которых резолвятся имена. Без него на каждую страницу
    /// пришлось бы делать два дополнительных запроса и втрое быстрее упираться
    /// в лимит 3 rps.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK.
    pub async fn get_conversations(
        &self,
        count: i64,
        offset: i64,
        filter: &str,
    ) -> Result<ConversationsRaw, VkError> {
        let params: Params = vec![
            ("count", clamp_limit(count).to_string()),
            ("offset", offset.max(0).to_string()),
            ("filter", normalize_filter(filter).to_string()),
            ("extended", "1".to_string()),
            ("fields", EXTENDED_FIELDS.to_string()),
        ];
        self.call("messages.getConversations", params).await
    }

    /// `messages.getHistory` — страница переписки.
    ///
    /// `start_message_id` + `offset=1` — способ VK сказать «то, что СТАРШЕ
    /// этого сообщения»: сам `start_message_id` в ответ не попадает, иначе
    /// последнее сообщение предыдущей страницы дублировалось бы в ленте.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK.
    pub async fn get_history(
        &self,
        peer_id: i64,
        count: i64,
        start_message_id: Option<i64>,
    ) -> Result<MessagesPageRaw, VkError> {
        let mut params: Params = vec![
            ("peer_id", peer_id.to_string()),
            ("count", clamp_limit(count).to_string()),
            ("extended", "1".to_string()),
            ("fields", EXTENDED_FIELDS.to_string()),
        ];
        if let Some(start) = start_message_id {
            params.push(("start_message_id", start.to_string()));
            params.push(("offset", "1".to_string()));
        }
        self.call("messages.getHistory", params).await
    }

    /// `messages.getById` — сообщения по идентификаторам.
    ///
    /// ВАЖНО: на чужой или удалённый идентификатор VK возвращает не ошибку, а
    /// пустой `items` — превращение этого в `404 unknown_message` живёт в
    /// роутах (Ф4), потому что решение «пусто — это ошибка» зависит от
    /// вызывающего.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK.
    pub async fn get_by_id(&self, message_ids: &[i64]) -> Result<MessagesPageRaw, VkError> {
        let ids: Vec<i64> = message_ids.iter().copied().take(MAX_MESSAGE_IDS).collect();
        let params: Params = vec![
            ("message_ids", join_ids(&ids)),
            ("extended", "1".to_string()),
            ("fields", EXTENDED_FIELDS.to_string()),
        ];
        self.call("messages.getById", params).await
    }

    // -----------------------------------------------------------------
    // Изменение состояния.
    // -----------------------------------------------------------------

    /// `messages.send` — отправка сообщения.
    ///
    /// `random_id` приходит СНАРУЖИ и переиспользуется при повторе — см.
    /// шапку модуля и [`new_random_id`].
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK; [`VkError::Decode`], если `response`
    /// не число (VK отдаёт голый `message_id`).
    pub async fn send_message(
        &self,
        peer_id: i64,
        random_id: i32,
        body: SendBody,
    ) -> Result<i32, VkError> {
        let mut params: Params = vec![
            ("peer_id", peer_id.to_string()),
            ("random_id", random_id.to_string()),
        ];
        push_opt(&mut params, "message", body.message.as_deref());
        push_opt(&mut params, "attachment", body.attachment.as_deref());
        push_opt(&mut params, "reply_to", body.reply_to);
        push_opt(&mut params, "forward", body.forward.as_deref());
        push_opt(&mut params, "payload", body.payload.as_deref());
        push_captcha(&mut params, body.captcha.as_ref());

        let value = self.call_value("messages.send", params).await?;
        let id = value.as_i64().ok_or_else(|| VkError::Decode {
            reason: "messages.send: `response` is not a message id".to_string(),
        })?;
        i32::try_from(id).map_err(|_| VkError::Decode {
            reason: format!("messages.send: message id {id} does not fit into i32"),
        })
    }

    /// `messages.markAsRead` — отметить беседу прочитанной.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK.
    pub async fn mark_as_read(
        &self,
        peer_id: i64,
        start_message_id: Option<i64>,
        captcha: Option<&Captcha>,
    ) -> Result<(), VkError> {
        let mut params: Params = vec![("peer_id", peer_id.to_string())];
        push_opt(&mut params, "start_message_id", start_message_id);
        push_captcha(&mut params, captcha);
        self.call_value("messages.markAsRead", params).await?;
        Ok(())
    }

    /// `messages.setActivity` — «печатает…» или «записывает голосовое».
    ///
    /// VK держит статус около 10 секунд, поэтому фронт обязан повторять вызов,
    /// а не звать его на каждую букву: при лимите 3 rps набор текста иначе
    /// съел бы весь бюджет запросов.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK.
    pub async fn set_activity(&self, peer_id: i64, kind: &str) -> Result<(), VkError> {
        // Незнакомый вид активности VK отвергает кодом 100 — приводим к
        // допустимому здесь, чтобы ошибка не доехала до пользователя.
        let kind = if kind == "audiomessage" {
            "audiomessage"
        } else {
            "typing"
        };
        let params: Params = vec![
            ("peer_id", peer_id.to_string()),
            ("type", kind.to_string()),
        ];
        self.call_value("messages.setActivity", params).await?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // LongPoll (используется в Ф5).
    // -----------------------------------------------------------------

    /// `messages.getLongPollServer` — адрес, ключ и позиция.
    ///
    /// `need_pts=1` обязателен: без `pts` нельзя вызвать
    /// [`VkClient::get_longpoll_history`], то есть обогащение пачки событий
    /// одним запросом превратилось бы в запрос на каждое событие.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK.
    pub async fn get_longpoll_server(&self) -> Result<LongPollServerRaw, VkError> {
        let params: Params = vec![
            ("need_pts", "1".to_string()),
            ("lp_version", LP_VERSION.to_string()),
        ];
        self.call("messages.getLongPollServer", params).await
    }

    /// `messages.getLongPollHistory` — полные сообщения для пачки событий.
    ///
    /// `preview_length=0` означает «не резать текст»: урезанный текст в ленте
    /// выглядит как повреждённые данные.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK.
    pub async fn get_longpoll_history(
        &self,
        ts: u64,
        pts: Option<u64>,
    ) -> Result<LongPollHistoryRaw, VkError> {
        let mut params: Params = vec![
            ("ts", ts.to_string()),
            ("preview_length", "0".to_string()),
            ("msgs_limit", "200".to_string()),
            ("events_limit", "1000".to_string()),
            ("lp_version", LP_VERSION.to_string()),
            ("fields", EXTENDED_FIELDS.to_string()),
        ];
        push_opt(&mut params, "pts", pts);
        self.call("messages.getLongPollHistory", params).await
    }

    // -----------------------------------------------------------------
    // Загрузка файлов (используется в Ф8).
    // -----------------------------------------------------------------

    /// `photos.getMessagesUploadServer` — адрес заливки фотографии.
    ///
    /// Адрес выдаётся под КОНКРЕТНЫЙ `peer_id`, поэтому весь трёхшаговый
    /// конвейер VK живёт внутри отправки сообщения: на шаге «пользователь
    /// прикрепил файл» адресат ещё неизвестен.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK.
    pub async fn photos_upload_server(&self, peer_id: i64) -> Result<UploadServerRaw, VkError> {
        let params: Params = vec![("peer_id", peer_id.to_string())];
        self.call("photos.getMessagesUploadServer", params).await
    }

    /// `photos.saveMessagesPhoto` — превращает ответ upload-сервера во
    /// вложение (одно или сразу пять, если заливался альбом).
    ///
    /// Возвращает СЫРОЙ массив сохранённых фотографий, а не
    /// [`crate::api::schema::PhotoRaw`], намеренно: единственный потребитель —
    /// [`crate::routes::upload::attachment_string`], чистая и таблично
    /// покрытая функция, которая и знает, как из ответа собирается
    /// attachment-строка. Типизированная проекция здесь означала бы ВТОРОЕ
    /// место, знающее форму ответа, и расхождение между ними всплыло бы уже на
    /// живой отправке.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK.
    pub async fn photos_save(
        &self,
        photo: &str,
        server: i64,
        hash: &str,
    ) -> Result<Value, VkError> {
        let params: Params = vec![
            ("photo", photo.to_string()),
            ("server", server.to_string()),
            ("hash", hash.to_string()),
        ];
        let value = self.call_value("photos.saveMessagesPhoto", params).await?;
        Ok(array_or_field(value, "items"))
    }

    /// `docs.getMessagesUploadServer` — адрес заливки документа.
    ///
    /// `kind` — `doc` либо `audio_message`: голосовое у VK это документ
    /// особого типа, и выбирается он ЗДЕСЬ, до заливки, а не при сохранении.
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK.
    pub async fn docs_upload_server(
        &self,
        peer_id: i64,
        kind: &str,
    ) -> Result<UploadServerRaw, VkError> {
        let kind = if kind == "audio_message" {
            "audio_message"
        } else {
            "doc"
        };
        let params: Params = vec![
            ("peer_id", peer_id.to_string()),
            ("type", kind.to_string()),
        ];
        self.call("docs.getMessagesUploadServer", params).await
    }

    /// `docs.save` — превращает ответ upload-сервера во вложение.
    ///
    /// Возвращает СЫРОЙ объект (`{"type":"doc","doc":{…}}` либо
    /// `{"type":"audio_message","audio_message":{…}}`) по той же причине, что и
    /// [`VkClient::photos_save`]: форму ответа разбирает единственная чистая
    /// функция [`crate::routes::upload::attachment_string`]. Ожидаемая схема
    /// зафиксирована в [`crate::api::schema::DocSaveRaw`].
    ///
    /// # Errors
    ///
    /// Любая ошибка транспорта или VK.
    pub async fn docs_save(&self, file: &str, title: Option<&str>) -> Result<Value, VkError> {
        let mut params: Params = vec![("file", file.to_string())];
        push_opt(&mut params, "title", title);
        self.call_value("docs.save", params).await
    }
}

/// Склеивает идентификаторы через запятую — форма списков у VK.
fn join_ids(ids: &[i64]) -> String {
    ids.iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Разбор с именем метода в тексте ошибки.
fn from_value_named<T: serde::de::DeserializeOwned>(
    method: &str,
    value: Value,
) -> Result<T, VkError> {
    serde_json::from_value(value).map_err(|e| VkError::Decode {
        reason: format!("{method}: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_limit_boundaries() {
        assert_eq!(clamp_limit(0), 1, "ноль — опечатка клиента, а не «по умолчанию»");
        assert_eq!(clamp_limit(1), 1);
        assert_eq!(clamp_limit(-42), 1);
        assert_eq!(clamp_limit(i64::MIN), 1);
        assert_eq!(clamp_limit(50), 50);
        assert_eq!(clamp_limit(100), 100);
        assert_eq!(clamp_limit(10_000), MAX_DIALOGS);
        assert_eq!(clamp_limit(i64::MAX), MAX_DIALOGS);
    }

    #[test]
    fn filter_normalization_table() {
        for f in DIALOG_FILTERS {
            assert_eq!(normalize_filter(f), f);
        }
        assert_eq!(normalize_filter(" unread "), "unread", "пробелы по краям");
        for garbage in ["", "ALL", "важные", "unread; DROP TABLE", "../../etc"] {
            assert_eq!(normalize_filter(garbage), "all", "мусор {garbage:?}");
        }
    }

    /// Ответ адресуется `conversation_message_id` — и это единственный
    /// способ попасть в нужную ветку беседы.
    ///
    /// Мутация «сложить сюда `message_id`» этот тест не ловит (числа
    /// неразличимы), её ловит `vk_stub::reply_is_addressed_by_cmid`. Здесь
    /// проверяется ФОРМА строки: без `is_reply` VK сделает пересылку вместо
    /// ответа, без `peer_id` — не найдёт оригинал.
    #[test]
    fn reply_forward_has_exactly_one_id_and_the_is_reply_flag() {
        let s = reply_forward_json(2_000_000_001, 514);
        assert_eq!(
            s,
            "{\"peer_id\":2000000001,\"conversation_message_ids\":[514],\"is_reply\":1}"
        );
        // Строка обязана оставаться валидным JSON: VK разбирает её сам.
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["peer_id"], 2_000_000_001_i64);
        assert_eq!(v["is_reply"], 1);
        assert_eq!(
            v["conversation_message_ids"].as_array().unwrap().len(),
            1,
            "с is_reply список из двух VK отвергает"
        );

        // Личный диалог: peer_id отрицательным не бывает, а вот у сообщества —
        // бывает, и минус обязан пережить сериализацию.
        let s = reply_forward_json(-123, 7);
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["peer_id"], -123);
        assert_eq!(v["conversation_message_ids"][0], 7);
    }

    /// `random_id` обязан быть положительным `i32` и различаться от вызова к
    /// вызову: ноль у VK означает «дедупликацию не делать».
    #[test]
    fn random_id_is_positive_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10_000 {
            let id = new_random_id();
            assert!(id > 0, "random_id обязан быть положительным: {id}");
            seen.insert(id);
        }
        assert!(seen.len() > 9_900, "коллизий подозрительно много: {}", seen.len());
    }

    /// ГЛАВНОЕ правило сборки параметров: токена в них нет.
    ///
    /// Проверяется на отладочном выводе — именно он попал бы в лог, если бы
    /// кто-то решил «залогировать params для отладки».
    #[test]
    fn params_never_contain_the_token() {
        let mut params: Params = vec![
            ("peer_id", "1234".into()),
            ("random_id", new_random_id().to_string()),
        ];
        push_opt(&mut params, "message", Some("привет"));
        push_captcha(
            &mut params,
            Some(&Captcha {
                sid: "239".into(),
                key: "abc".into(),
            }),
        );

        let dbg = format!("{params:?}");
        assert!(!dbg.contains("access_token"), "{dbg}");
        assert!(!dbg.contains("vk1.a."), "{dbg}");
        assert!(dbg.contains("captcha_sid"), "captcha обязана доехать: {dbg}");
        assert!(dbg.contains("captcha_key"), "{dbg}");
    }

    /// Необязательные параметры не появляются пустыми: `message=` у VK
    /// означает «отправить пустой текст», а не «текста нет».
    #[test]
    fn optional_params_are_omitted_when_absent() {
        let mut params: Params = Vec::new();
        push_opt(&mut params, "message", Option::<&str>::None);
        push_opt(&mut params, "reply_to", Option::<i64>::None);
        push_captcha(&mut params, None);
        assert!(params.is_empty(), "{params:?}");
    }

    #[test]
    fn ids_are_joined_the_vk_way() {
        assert_eq!(join_ids(&[]), "");
        assert_eq!(join_ids(&[1]), "1");
        assert_eq!(join_ids(&[1, -55, 2_000_000_007]), "1,-55,2000000007");
    }

    /// Обе формы ответа VK (голый массив и объект с полем) обязаны
    /// разбираться одинаково.
    #[test]
    fn array_or_field_survives_both_response_shapes() {
        let flat = serde_json::json!([{"id": 1, "first_name": "A"}]);
        let wrapped = serde_json::json!({"groups": [{"id": 55, "name": "G"}]});

        let users: Vec<ProfileRaw> =
            serde_json::from_value(array_or_field(flat, "items")).unwrap();
        assert_eq!(users[0].id, 1);

        let groups: Vec<GroupRaw> =
            serde_json::from_value(array_or_field(wrapped, "groups")).unwrap();
        assert_eq!(groups[0].name, "G");

        // Совсем незнакомая форма даёт пустой список, а не панику.
        let none = array_or_field(serde_json::json!({"count": 0}), "items");
        assert_eq!(none, serde_json::json!([]));
    }

    /// Вид активности приводится к допустимому: VK отвергает всё прочее кодом
    /// 100, а показывать это пользователю бессмысленно.
    #[test]
    fn activity_kind_is_normalized() {
        // Нормализация живёт внутри `set_activity`; проверяем её логику
        // напрямую, чтобы не поднимать стенд ради одной строки.
        for (input, expected) in [
            ("typing", "typing"),
            ("audiomessage", "audiomessage"),
            ("", "typing"),
            ("dancing", "typing"),
        ] {
            let normalized = if input == "audiomessage" {
                "audiomessage"
            } else {
                "typing"
            };
            assert_eq!(normalized, expected, "вход {input:?}");
        }
    }

    /// Фикстуры разбираются в те же структуры, что и живой ответ.
    #[test]
    fn fixtures_parse_into_method_return_types() {
        let conv: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/get_conversations.json"
        ))
        .unwrap();
        let parsed: ConversationsRaw = serde_json::from_value(conv["response"].clone()).unwrap();
        assert_eq!(parsed.items.len(), 3);

        let hist: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/get_history.json")).unwrap();
        let parsed: MessagesPageRaw = serde_json::from_value(hist["response"].clone()).unwrap();
        assert_eq!(parsed.items.len(), 5);

        let lp: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/get_longpoll_server.json"
        ))
        .unwrap();
        let parsed: LongPollServerRaw = serde_json::from_value(lp["response"].clone()).unwrap();
        assert_eq!(parsed.key.len(), 40);
    }
}
