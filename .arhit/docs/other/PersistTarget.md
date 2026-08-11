# PersistTarget

PersistTarget (plugins/vk/src/analysis/hub.rs) — куда пампер кладёт полный текст УСПЕШНОГО run'а анализа чата VK. Два варианта: Analysis{last_message_id, messages_used} (kind analyze/refresh -> PromptStore::set_analysis) и Turn{question} (kind ask -> PromptStore::push_turn, вопрос станет q новой пары треда).

Enum, а не два Option-поля в PersistCtx: у разбора и у пары треда нет общих параметров, и Option на каждый допускал бы состояния «разбор без точки отсчёта» и «пара без вопроса», которых не бывает.

Сам PersistCtx (store + path + dialog_id + target) собирается в AnalysisHub::start и уходит в PumpCtx. None вместо него означает «сохранять нечего или незачем»: пустой транскрипт (нет newest_id), ask без вопроса, hub без подключённого хранилища. Запись происходит только при ok == true и непустом накопителе — см. persist_result.
