# Ф3 мультипортальности Битрикс-плагина — экстрактор Portal и перевод роутов

## Что изменилось

До Ф3 роуты плагина были однопортальными: build_router принимал Arc<B24State>, а register_routes биндил их на registry.primary(). Роутер собирается один раз при старте, поэтому add/remove портала в рантайме (Ф4) внесли бы реальный баг — роуты продолжили бы смотреть на старый Arc.

Ф3 сняла это ограничение:
- состоянием Router стал Arc<B24Registry>;
- портал выбирает КАЖДЫЙ запрос — экстрактором Portal(Arc<B24State>) с impl FromRequestParts<Arc<B24Registry>>;
- register_routes больше не зовёт primary().

## Файлы

- plugins/bitrix/src/routes/portal.rs — НОВЫЙ. Экстрактор Portal, константы PORTAL_HEADER ("x-forge-portal") и PORTAL_QUERY ("portal"), Portal::resolve, 10 юнит-тестов.
- plugins/bitrix/src/routes/error.rs — два новых кода в СУЩЕСТВУЮЩЕМ конверте: ApiError::unknown_portal (404) и ApiError::no_portal (503). Таблица в шапке модуля дополнена.
- plugins/bitrix/src/routes/{mod,login,dialogs,tasks,media,upload,analysis,voice}.rs — 36 хендлеров переведены на Portal(state): Portal, семь суб-роутеров — на Router<Arc<B24Registry>>.
- plugins/bitrix/src/ws/mod.rs — bitrix_ws на Portal; в B24WsQuery добавлено поле portal. Агрегация кадров всех порталов — работа Ф5.
- plugins/bitrix/src/registry.rs — B24Registry::from_states / ::single.
- plugins/bitrix/src/lib.rs — register_routes передаёт реестр целиком.
- plugins/bitrix/tests/portal_isolation.rs — НОВЫЙ. 9 HTTP-тестов изоляции.

## Порядок резолва портала

X-Forge-Portal → ?portal= → первый СЛОТ реестра. Неизвестный id — 404 unknown_portal. Вход нормализуется config::normalize_portal_id.

## Инварианты, которые фаза обязана была сохранить (и сохранила)

1. Порядок проверок в data-роутах не изменился: авторизация (409) → соединение (503) → резолв объекта (404) → разбор параметров (400). Резолв портала встал ПЕРЕД всей цепочкой. Действующий страж hostile_dialog_ids_hit_auth_before_any_parsing зелёный без правок; добавлен пер-портальный аналог the_check_ladder_still_starts_with_409_per_portal.
2. Экстрактор Portal — единственное исключение из правила «никаких экстракторов до тела хендлера», и он не имеет права отвечать 400 ни на каком вводе (тест the_extractor_never_answers_400).
3. Query по-прежнему разбирается вручную; Portal тоже разбирает ?portal= вручную через dialogs::query_str, а не через Query<T>.
4. register_routes зовётся до .fallback(...) и .layer(auth_middleware) хоста.
5. WebhookUrl без Serialize; ни вебхука, ни отпечатка, ни хоста портала в ответах роутов нет (проверено в status_reports_the_state_of_the_addressed_portal).
6. Таблица замороженного контракта путей не менялась; tmux-web/tests/frontend/bitrix_contract.test.mjs — 28/28.

## Изоляция

tests/portal_isolation.rs держит ДВА герметичных стенда портала на 127.0.0.1:0 с ОДИНАКОВЫМИ dialog_id (chat7), message_id (101) и task_id (42) — различается только содержимое. Разведи их идентификаторами, и тест проходил бы даже при перепутанном состоянии.

Проверяется: список диалогов, лента сообщений, список задач и карточка, локальная заметка анализа (роут до портала не доходит вовсе — промах экстрактора виден сразу), детерминированный фолбэк без заголовка, ?portal= как эквивалент заголовка, 404 unknown_portal без единого похода в портал, /status по адресуемому порталу, лестница 409 пер-портально.

Мутационная проверка: если экстрактор начнёт игнорировать заголовок и всегда отдавать первый слот, падают 8 из 9 тестов.

## Что осталось следующим фазам

- Ф4: роуты управления порталами; login/logout пока пишут секрет через однопортальный save_webhook_to / clear_webhook_to по state.config.config_path — это Ф4.4.
- Ф5: /ws/bitrix пока пер-портальный (один сокет = один портал), агрегация кадров всех порталов и поле portal_id — Ф5.
- Ф6: cache_dir и prompts_path у порталов пока разводит конфигурация, а не код.
- Ф8.2: фронтовая константа заголовка обязана быть дословно "X-Forge-Portal".