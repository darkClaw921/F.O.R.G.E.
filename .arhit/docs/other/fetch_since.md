# fetch_since

plugins/vk/src/analysis/context.rs — выгрузка ТОЛЬКО сообщений новее уже разобранных, вход инкрементального обновления (kind=refresh).

Сигнатура: async fn fetch_since(state, client, peer, since_id: i32, cap: i64) -> Result<NewMessages, ApiError>.

Тот же постраничный цикл, что у fetch (общий шаг вынесен в fetch_page: get_history → peers.absorb_extended → resolve_names → проекция в MessageDto), те же потолки MAX_PAGES и clamp_depth. Два отличия:
1. Цикл останавливается на границе: как только на странице встретилось сообщение с id <= since_id, листать дальше нечего (история идёт от новых к старым). Поэтому обычный REFRESH стоит ОДНОГО вызова VK вместо пяти.
2. Переполнение объявляется: если новых больше cap или они не уместились в MAX_PAGES, берутся самые НОВЫЕ, а NewMessages.truncated = true.

NewMessages { messages: Vec<MessageDto> (возрастание id, без дублей), truncated: bool }. Ни одного сообщения с id <= since_id в результате не бывает — тест fetch_since_returns_only_the_newer_messages в plugins/vk/tests/vk_stub.rs.

Роут POST /api/vk/dialogs/:id/analysis при kind=refresh вызывает её с since_id = ChatAnalysis::last_message_id; пустой транскрипт из результата даёт штатный ответ no_new_messages без запуска модели.
