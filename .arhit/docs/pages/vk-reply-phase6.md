ФАЗА 6 плагина VK — ответы и цитаты (эпик forge-vk-f6-hfy2).

ЧТО СДЕЛАНО
1. Бэкенд (dto.rs уже собирал ReplyToDto из инлайненного reply_message с Ф4): добавлена адресация ответа по conversation_message_id. SendMessageReq.reply_to_cmid -> SendBody.forward = reply_forward_json(peer, cmid) = {"peer_id":P,"conversation_message_ids":[C],"is_reply":1}; при заданном cmid параметр reply_to не отправляется вовсе. reply_to_id остаётся для случая, когда cmid у фронта нет.
2. tmux-web/static/js/messenger/follow-core.js (новый) — общий слой «следования» ФАБРИКОЙ createFollow({ttlMs, scrollPad, cancelUpPx}); часы инжектируются параметром nowMs. Telegram НЕ мигрирован намеренно.
3. tmux-web/static/js/vk/reply.js (новый) — копия-адаптация telegram/reply.js с ДРУГИМ ключом цели (cmid).
4. tmux-web/static/js/vk/chat.js — полоса ответа, цитата в строке, переход к оригиналу, вторая карта строк rowsByCmid, следование через createFollow(), send(text, replyToCmid, payload).
5. tmux-web/static/js/vk/api.js — sendMessage({replyToCmid}) -> тело reply_to_cmid.
6. tests/frontend/vk_dom_stub.mjs — панель собрана НАСТОЯЩИМ деревом #vk-session (полоса ответа встаёт insertBefore(#vk-prompt), и без родителя ensureReplyBar молча возвращал бы null).

ГЛАВНЫЙ ВЫИГРЫШ VK ПРОТИВ TELEGRAM: страница сообщений стоит ОДИН запрос. У Telegram превью оригиналов добираются вторым RPC (~250 строк обогащения в его dto.rs), у VK reply_message инлайнится в сообщение. Щит — vk_stub::a_page_of_replies_costs_exactly_one_call (50 сообщений, 25 ответов -> ровно 1 messages.getHistory, 0 messages.getById; фикстура намеренно несёт глобальный id у reply_message, иначе тест не заметил бы возвращения дозапроса).

ПОЧЕМУ ФАБРИКА, А НЕ СИНГЛТОН: peer_id беседы VK 2000000001 и dialog_id супергруппы Telegram 2000000001 — разные чаты, а состояние сравнивает их строкой; общий инстанс на две панели тянул бы ленту не той панели.

ПОЧЕМУ reply.js СКОПИРОВАН, А НЕ ОБОБЩЁН: у VK ответ адресуется conversation_message_id — другой инвариант, а не параметризация имени поля (cmid нумеруется в каждой беседе с единицы; у инлайненного оригинала глобального id может не быть).

МУТАЦИОННОЕ ТЕСТИРОВАНИЕ (все мутации откачены):
- follow-core как модульный синглтон -> messenger_follow_factory: 4 [FAIL] в разделе независимости инстансов;
- ключ цели message_id вместо cmid -> vk_reply: 7 провалов, vk_reply_ui: 10;
- не сбрасывать цель после успешной отправки -> vk_reply_ui: 2;
- вернуть дозапрос за оригиналами -> vk_stub::a_page_of_replies_costs_exactly_one_call FAILED (getById 1 вместо 0);
- слать cmid в параметр reply_to -> vk_stub::reply_is_addressed_by_cmid_not_by_message_id FAILED;
- фронт кладёт cmid в reply_to_id -> vk_reply_ui: 3.

ТЕСТЫ ФАЗЫ: vk_reply (102), vk_reply_ui (89), vk_scroll_ui (43), vk_scroll_edge (28), messenger_follow_factory (94) — плюс все прежние frontend-тесты и cargo test --workspace зелёные, clippy чист, cargo tree -i openssl-sys пусто.

НЕ ТРОНУТО: js/telegram/*, css/telegram.css, plugins/telegram/** (git diff по ним пуст).