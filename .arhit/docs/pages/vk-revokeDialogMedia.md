tmux-web/static/js/vk/media.js::revokeDialogMedia() -> number и dropMessageMedia(dialogId, messageId) -> number (Ф7) — инвариант «каждый созданный objectURL отозван».

revokeDialogMedia() отпускает ВСЕ блобы вкладки и чистит vkState.mediaCache + реестр незавершённых загрузок. Зовётся при смене беседы (chat.js::openDialog), из teardownMedia (logout, выгрузка страницы). Без него двадцать переходов по беседам с фотографиями остаются в памяти вкладки до перезагрузки: браузер не собирает objectURL сам, а у VK ОДНО сообщение способно принести десять блобов.

dropMessageMedia(dialogId, messageId) выбрасывает вложения одного сообщения — все индексы 0..MAX_ATTACH_INDEX и оба вида. Обходятся ВСЕ индексы, а не «сколько было вложений»: правка могла заменить список, и старую длину спросить уже негде. Зовётся на message_edited и message_deleted, одновременно с серверной media_cache::invalidate.

Проверяется СЧЁТЧИКОМ в vk_media.test.mjs (objectUrls.created === objectUrls.revoked), а не «вроде бы вызвали revoke»; мутация «убрать revoke при смене беседы» краснит тест.