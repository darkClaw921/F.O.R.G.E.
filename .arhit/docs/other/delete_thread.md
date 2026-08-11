# delete_thread

delete_thread — handler DELETE /api/vk/dialogs/:id/analysis/thread (plugins/vk/src/routes/analysis.rs), «CLEAR Q&A» на проводе.

Лестница: require_authorized (409 not_authorized) -> resolve_peer (404 unknown_dialog) -> VkState::clear_chat_thread. Ступени «соединение» (require_ready) НЕТ — тред лежит в локальном файле, и 503 на его очистку был бы враньём; та же ступень, что у GET/DELETE /analysis и /prompt.

Ответ: 200 {"cleared": bool} — был ли тред непустым. Пустой тред и беседа, которой нет в файле, дают cleared:false, а не 404: повторное нажатие кнопки и вторая вкладка — нормальные состояния. Ошибка записи файла — 500 storage_failed общим механизмом ApiError, без текста переписки в сообщении.

Почему ОТДЕЛЬНЫЙ путь, а не DELETE /analysis?thread=1: query-параметры в плагине разбираются вручную через RawQuery — типовой экстрактор Query<T> отрабатывает ДО тела хендлера и отвечает 400 раньше проверки авторизации, то есть ломает auth-ladder (та же причина, по которой тела здесь Option<Json<T>>). Отдельный маршрут попадает в сторожевые тесты routes/mod.rs перебором наравне с остальными: data_routes_are_mounted_and_demand_authorization и hostile_dialog_ids_hit_auth_before_any_parsing.

Инвариант: DELETE /analysis стирает только разбор, DELETE /analysis/thread — только тред, пустой PUT /prompt — только заметку. Ни одна очистка не задевает две другие половины записи.
