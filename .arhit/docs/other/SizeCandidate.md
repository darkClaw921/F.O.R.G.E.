# SizeCandidate

plugins/vk/src/attach.rs::SizeCandidate {letter: char, width: i32, height: i32} (Ф7) — один вариант картинки в терминах, не зависящих от формы ответа VK.

Нужен потому, что у фотографии, превью документа, кадра видео и картинки стикера структуры РАЗНЫЕ (у стикера буквенного кода нет вовсе), а правило выбора одно. Проекции: photo_candidates(&[PhotoSizeRaw]) и sticker_candidates(&[StickerImageRaw]). letter = 0 означает «буквенного кода нет». Используется pick_size; см. THUMB_MIN_PX = 200.
