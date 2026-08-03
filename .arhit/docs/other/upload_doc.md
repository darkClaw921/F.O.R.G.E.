# upload_doc

Заливает один документ или голосовое сообщение (plugins/vk/src/routes/upload.rs): docs.getMessagesUploadServer(peer_id, type=doc|audio_message) [троттлится] -> POST upload_url полем file [не троттлится] -> docs.save [троттлится] -> attachment_string. Тип документа задаётся ПРИ ВЫДАЧЕ АДРЕСА, а не при сохранении: перепутав его, получаешь обычный файл .ogg вместо кружка с осциллограммой. Заголовок title у docs.save — исходное имя файла из UploadEntry.
