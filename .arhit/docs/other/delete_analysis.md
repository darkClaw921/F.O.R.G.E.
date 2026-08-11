# delete_analysis

DELETE /api/vk/dialogs/:id/analysis (plugins/vk/src/routes/analysis.rs) — стереть сохранённый разбор.

Та же лестница, что у GET: авторизация + resolve_peer, без require_ready. Заметку пользователя НЕ трогает — в попапе это две разные кнопки (CLEAR анализа и CLEAR Q&A/заметки), а в хранилище две независимые половины ChatPrompt.

Ответ ClearAnalysisResp { cleared: bool }. Разбора не было — 200 {cleared:false}, а не 404: две вкладки могут нажать «очистить» подряд. Ошибка записи файла — 500 storage_failed.

Ключ хранилища канонический (peer_id после resolve_peer), поэтому /dialogs/1234/... и /dialogs/%201234%20/... чистят одну и ту же запись.
