# persist_result

persist_result(&PersistCtx, text) (plugins/vk/src/analysis/hub.rs) — записывает результат успешного run'а анализа чата VK в хранилище: мапа и файл под ОДНИМ lock'ом, как в VkState::set_chat_prompt. Бывший persist_analysis, расширенный на тред.

Ветвление по PersistCtx::target: Analysis -> set_analysis (разбор + точка отсчёта следующего REFRESH), Turn -> push_turn (пара «вопрос — ответ» в конец треда). Пустой (после trim) текст не сохраняется ни в одну из веток: это run, из которого не приехало ни одной дельты.

Зовётся только из pump и только при ok == true и не взведённом токене отмены. Ошибка записи НЕ роняет run и не превращается в analysis_done{ok:false}: текст уже целиком у пользователя на экране. В лог уходят только размеры и метка what (analysis | qa turn) — и разбор, и ответ на вопрос это пересказ чужой переписки.
