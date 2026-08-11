Бэкенд «Анализа чата через Claude» в plugins/telegram (эпик forge-caport-tg-j1x4, Этап 2).

СОСТАВ:
- plugins/telegram/Cargo.toml: подключены forge-chat-analysis (общее ядро фичи) и forge-claude-runner (стриминговый раннер claude -p). Оба — листовые крейты, общие с VK и Bitrix; копия здесь разъехалась бы с оригиналом на первом же исправлении.
- plugins/telegram/src/analysis/mod.rs: переэкспорт ядра (hub, prompt, prompts, AnalysisHub, AnalysisSink, AnalysisError, StartRequest, KIND_*, REASON_*) под путями crate::analysis::* — то же соглашение, что у VK.
- plugins/telegram/src/analysis/context.rs: ЕДИНСТВЕННОЕ, что говорит с Telegram. Выгрузка истории (fetch/fetch_since поверх трейта HistorySource; продовая реализация ClientSource = client.iter_messages(peer).limit(n).offset_id(before)) и ЧИСТЫЙ рендер транскрипта (render) из MessageDto.
- plugins/telegram/src/routes/analysis.rs: 7 роутов / 8 путей, две лестницы проверок, канонический ключ хранилища, мапинг AnalysisError -> ApiError.
- plugins/telegram/src/routes/error.rs: +claude_unavailable (409, с путём и именем поля конфига) и +storage_failed (500).
- plugins/telegram/src/ws/protocol.rs: 4 варианта TgServerMsg (AnalysisStarted/Delta/Done/Error) + TgAnalysisSink (impl forge_chat_analysis::AnalysisSink).
- plugins/telegram/src/config.rs: prompts_path, claude_cli_path, analysis_timeout_secs, analysis_max_chars, analysis_model.
- plugins/telegram/src/state.rs: поля prompts (Arc<Mutex<PromptStore>>) и analysis (AnalysisHub), analysis_sink() и обёртки chat_prompt/set_chat_prompt/chat_analysis/clear_chat_analysis/chat_thread/clear_chat_thread.
- plugins/telegram/src/lib.rs: state.analysis.shutdown() в shutdown().

КЛЮЧЕВЫЕ РЕШЕНИЯ И ЛОВУШКИ:
1. offset_id ЭКСКЛЮЗИВЕН. Курсором следующей страницы служит МИНИМАЛЬНЫЙ id предыдущей. min_id-1 терял бы ровно одно сообщение на каждом стыке страниц, min_id+1 — задваивал бы его. Закреплено тестом paging_loses_and_duplicates_nothing_on_the_seam.
2. Трейт HistorySource введён РАДИ ТЕСТИРУЕМОСТИ: логика пагинации (курсор, MAX_PAGES, обрезка по бюджету) иначе проверялась бы только живым аккаунтом. ClientSource не содержит ни одного if.
3. Ключ хранилища КАНОНИЧЕСКИЙ: dto::dialog_id_string(peer.id) после resolve_peer, а не сырой сегмент URL. У Telegram resolve_peer async и отдаёт PeerRef — нужна обратная конвертация. Без неё /dialogs/777000/prompt и /dialogs/%20777000%20/prompt завели бы ДВЕ заметки на один диалог, и разбор «пропадал» бы после перезахода.
4. ДВЕ ЛЕСТНИЦЫ на одном пути: GET/DELETE /analysis и оба /prompt работают с локальным файлом и require_ready НЕ требуют (иначе разбор прятался бы ровно тогда, когда сеть отвалилась). POST /analysis и /analysis/cancel требуют — разбор начинается с выгрузки истории.
5. Тело хендлеров — Option<Json<T>>, никогда Json<T>, и никакого Query<T>: экстрактор отработал бы ДО авторизации и превратил бы роут в оракул валидности id. Закреплено тестом every_analysis_route_hits_auth_before_any_parsing.
6. Порядок 4 кадров в TgServerMsg ЗАМОРОЖЕН (вставлены между FoldersChanged и Resync) и зеркалится в SERVER_FRAMES в js/telegram/ws.js. Сверяется и rust-тестом every_server_variant_has_a_sample (assert_eq по слайсам, а не по составу), и telegram_contract.test.mjs.
7. РАЗЛИЧИЕ С VK: '+123' у Telegram — ЗАКОННОЕ написание 123 (parse_dialog_id идёт через i64::from_str, а тот принимает знак), поэтому ответ 503 not_connected, а не 404. У VK тот же id даёт 404. Зафиксировано тестом a_signed_id_is_a_legal_spelling_not_garbage.
8. РЕНДЕР ПРОЩЕ VK: у Telegram нет attachments (медиа ровно одно, Option<MediaDto>), нет fwd, нет action, нет cmid — эти ветки отсутствуют, а не закомментированы. Зато sender уже заполнен проекцией, поэтому резолв имён (самая дорогая часть VK-выгрузки) не нужен вовсе.
9. ИНВАРИАНТ: в транскрипте НЕТ ни одного URL — медиа превращается в метки ([фото], [документ: смета.xlsx], [голосовое 0:14]). Здесь правило строже, чем у DTO: транскрипт уезжает в ЧУЖОЙ процесс.
10. Имя диалога для шапки промпта берётся ИЗ УЖЕ ВЫГРУЖЕННЫХ сообщений (title_from_messages — первый входящий автор), а не отдельным RPC: кэш peer'ов Telegram хранит PeerRef без title'ов.
11. FORGE_TELEGRAM_PROMPTS_PATH обязателен: без него cargo test писал бы в живой ~/.forge/telegram-chat-prompts.json пользователя. prompts_path помечен serde(skip) — TOML им не управляет, в отличие от claude_cli_path.
12. analysis.shutdown() в teardown НЕ покрывается shutdown_workers: пампер run'а — таска ХАБА, а сам claude вообще чужой процесс.

ТЕСТЫ: 508 зелёных в cargo test -p forge-telegram (было 465). 23 в analysis::context (пагинация, стык страниц, MAX_PAGES, застрявший курсор, fetch_since, обрезка бюджетом, метки медиа, цитаты, отсутствие URL), 20+ в routes::analysis (обе лестницы, канонический ключ, независимость очисток, storage_failed без утечки текста, мапинг ошибок ядра, дедуп и лимит на уровне хаба), плюс кадры в ws::protocol и поля в config.