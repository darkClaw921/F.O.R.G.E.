# uploadCardKind

Переводит ветку конвейера в вид КАРТОЧКИ (tmux-web/static/js/vk/format.js). Две системы имён встречаются ровно здесь: uploadKind отвечает «каким методом файл уедет в VK» (photo|video|doc|audio_message — по нему же берётся лимит из UPLOAD_LIMITS), а mediaGlyph/mediaKindLabel знают словарь ПОЛУЧЕННЫХ вложений (photo|video|document|voice). Без перевода документ рисовался бы глифом «◌ DATA», то есть «мы не поняли, что это», хотя всё понято. Соответствие: photo->photo, video->video, audio_message->voice, всё прочее->document.
