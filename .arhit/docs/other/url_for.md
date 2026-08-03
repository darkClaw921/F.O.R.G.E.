# url_for

plugins/vk/src/attach.rs::url_for(att: &AttachmentRaw, kind: MediaKind) -> Option<String> (Ф7). ЧИСТАЯ и ЕДИНСТВЕННАЯ точка, где адрес вложения покидает модуль.

ТАБЛИЦА (thumb / full):
  photo         — sizes[] по pick_size / sizes[] максимум
  doc           — preview.photo.sizes[] / doc.url
  audio_message — нет / link_ogg, иначе link_mp3
  audio         — нет / audio.url (VK отдаёт его не всегда — правообладатели)
  sticker       — images[] по pick_size / images_with_background[] максимум (прозрачный PNG на светлой теме невидим)
  graffiti      — url / url
  video         — image[], иначе first_frame[] / НЕТ ФАЙЛА
  link и прочее other — превью ссылки, если есть / нет

ВИДЕО: full всегда None. video.get отдаёт player — адрес СТРАНИЦЫ, прямые files.* доступны только владельцу ролика. Проксировать видеофайл мы не можем и не должны делать вид, что можем.

БЕЗОПАСНОСТЬ: http_url() пропускает только схемы http/https — адрес уходит прямо в VkClient::get_stream, и file:// там означал бы чтение локальных файлов сервера. http разрешён наравне с https ради интеграционного стенда tests/vk_stub.rs.

None означает «файла нет» — роут отвечает 404 no_media.
