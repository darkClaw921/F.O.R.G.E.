# push_turn

PromptStore::push_turn(dialog_id, q, a, now) -> Option<QaTurn> (plugins/vk/src/analysis/prompts.rs).

Добавляет пару «вопрос — ответ» в конец треда беседы, заводя запись беседы, если её ещё нет (как это делает set_analysis).

Что делает по шагам:
1. прогоняет q и a через sanitize_turn (потолок MAX_TURN_CHARS = 5 000 code points на ПОЛЕ; обрезка по символам, не по байтам — байтовый срез на кириллице и emoji паникует посреди символа);
2. пустой вопрос ЛИБО пустой ответ -> None, пара не добавляется (пустой накопитель это оборванный run, а не ответ);
3. кладёт QaTurn{q, a, at: now} в конец ChatPrompt.thread;
4. двигает updated_at записи — тред считается активностью, иначе беседа, где сегодня задавали вопросы, вылетела бы по MAX_CHATS из-за старой заметки;
5. clamp_thread обрезает тред со СТАРЫХ пар сразу по двум осям: не больше MAX_THREAD_TURNS = 12 пар и не больше MAX_THREAD_CHARS = 10 000 суммарных символов (q + a по всем парам). Последняя пара не выбрасывается никогда — этот инвариант держит MAX_TURN_CHARS = MAX_THREAD_CHARS / 2;
6. evict() вытесняет самые старые беседы сверх MAX_CHATS.

Инвариант: не трогает ни ChatPrompt.prompt (заметку), ни ChatPrompt.analysis (разбор). Проверяется тестами the_note_the_analysis_and_the_thread_are_independent и the_thread_round_trips_through_the_file (после реального save+load).

Кто зовёт: analysis::hub::persist_result по успешному run'у kind == ask (PersistTarget::Turn) и VkState через общий Mutex хранилища.
