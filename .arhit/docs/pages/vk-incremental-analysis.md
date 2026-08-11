# VK: сохраняемый и инкрементальный анализ чата (бэкенд)

Доработка фичи «Анализ чата через Claude» по итогам живого использования. Причины: вопрос по чату стирал разбор (он жил только в DOM попапа), и не было способа догнать переписку по новым сообщениям.

## Что изменилось

1. **Хранилище** (plugins/vk/src/analysis/prompts.rs). ChatPrompt получил поле analysis: Option<ChatAnalysis> {text, last_message_id, messages_used, updated_at}. Версия файла осталась 1, поле опциональное + skip_serializing_if — старые файлы читаются и переписываются без потерь. Заметка и разбор независимы во всех операциях. MAX_ANALYSIS_CHARS = 12 000 code points; из-за него MAX_FILE_BYTES поднят с 4 МиБ до 40 МиБ (худший случай 500 × 16 000 символов × 4 байта = 32 МБ; снижать вместо этого лимит разбора нельзя — в 4 МиБ поместился бы разбор в ~1 500 символов).

2. **Транскрипт** (analysis/context.rs). Transcript.newest_id + fetch_since(state, client, peer, since_id, cap) -> NewMessages{messages, truncated}. Постраничный цикл общий с fetch (вынесен в fetch_page), останавливается на первом уже разобранном сообщении — обычный REFRESH стоит одного вызова VK.

3. **Промпт** (analysis/prompt.rs). Task::Refresh { previous } и блок <previous_analysis> перед <transcript>; previous экранируется той же escape и режется по MAX_ANALYSIS_CHARS. Текст задачи требует ОБНОВЛЁННЫЙ ЦЕЛЬНЫЙ разбор, а не список изменений: ответ заменяет сохранённый целиком. SYSTEM_PROMPT называет previous_analysis данными.

4. **Hub** (analysis/hub.rs). Пампер копит полный ответ ОТДЕЛЬНЫМ аккумулятором (буфер коалесцинга флашится и обнуляется), лимит — MAX_ANALYSIS_CHARS, счётчик символов вместо пересчёта. Сохранение — только при ok == true и kind ∈ {analyze, refresh}; отмена/таймаут/ошибка не сохраняют НИЧЕГО (иначе огрызок стал бы «прошлым разбором»); ask не трогает разбор вовсе. Hub держит Arc на ту же PromptStore, что и VkState (AnalysisHub::with_store).

5. **Роуты** (routes/analysis.rs). GET и DELETE /api/vk/dialogs/:id/analysis на лестнице /prompt (без require_ready); POST принимает kind=refresh; AnalysisResp.run_id стал Option<String>, добавлено no_new_messages. Таблица-контракт в routes/mod.rs дополнена, auth-тесты покрывают новые методы (включая DELETE).

## Ключевые инварианты, закреплённые тестами

* an_old_file_without_analysis_round_trips — совместимость формата;
* a_cancelled_run_leaves_the_stored_analysis_alone, a_failed_run_stores_nothing, an_ask_run_never_touches_the_stored_analysis — что НЕ попадает в хранилище;
* an_overlong_answer_is_stored_truncated_but_streamed_whole — экран и файл живут по разным лимитам;
* fetch_since_* (tests/vk_stub.rs) — только новое, пусто на границе, обрезка по самым новым;
* refresh_without_new_messages_never_starts_the_model — штатный no_new_messages;
* a_refresh_feeds_the_model_only_the_new_messages_and_replaces_the_analysis — сквозной сценарий на живом HTTP-стенде.

## Что осталось фронту (эпик forge-vkinc-e2-pd5k)

js/vk/api.js (шапка контракта + getAnalysis/deleteAnalysis/refresh), разметка кнопок, разделение вывода анализа и Q&A-треда. До его завершения tmux-web/tests/frontend/vk_contract.test.mjs красный по таблице путей — это ожидаемо.