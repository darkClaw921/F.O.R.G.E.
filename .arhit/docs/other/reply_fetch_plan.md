# reply_fetch_plan

plugins/telegram/src/routes/dialogs.rs

`pub fn reply_fetch_plan(messages: &[MessageDto]) -> Vec<i32>`

Какие оригиналы цитат надо добрать по сети для этой страницы истории. Чистая функция.

Тонкая обёртка над dto::reply_ids_to_fetch (он уже дедуплицирует id и выбрасывает оригиналы,
лежащие на самой странице) + `truncate(dto::REPLY_FETCH_BATCH)`.

Обрезка — СТРАХОВКА, а не рабочий режим: при `MAX_MESSAGES <= REPLY_FETCH_BATCH` она недостижима,
и это соотношение сторожит `const _: () = assert!(...)` рядом с функцией. Обрезка стоит здесь ради
явного инварианта «на страницу истории — не более ОДНОГО дополнительного RPC»: лишний round-trip
виден пользователю на каждом экране прокрутки вверх.

СВЯЗИ: dto::reply_ids_to_fetch, dto::REPLY_FETCH_BATCH, MAX_MESSAGES, enrich_page_replies.
