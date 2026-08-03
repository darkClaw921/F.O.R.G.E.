# plugins/vk/tests/fixtures

Корпус фикстур ответов VK для forge-vk: get_conversations.json (3 беседы — личка, групповой чат с обложкой и push_settings, сообщество с can_write.reason; 2 профиля, 1 сообщество), get_history.json (5 сообщений: простое, с двухуровневой пересылкой БЕЗ id/peer_id, с инлайновым reply_message, с фото и голосовым, служебное chat_invite_user), get_by_id.json (сообщение с инлайн-клавиатурой из трёх кнопок text/callback/open_link), get_longpoll_server.json (server без схемы + pts), get_longpoll_history.json (разнородные события, messages/conversations/profiles/new_pts), error_auth_failed.json (код 5 с request_params, содержащими access_token).

Подключаются через include_str! из api/schema.rs и api/methods.rs и через Reply::Response в tests/vk_stub.rs. Собраны вручную по документации VK: снять их с живого аккаунта и обезличить можно будет после Ф3.
