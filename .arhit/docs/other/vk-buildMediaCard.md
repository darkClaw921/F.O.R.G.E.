# vk-buildMediaCard

tmux-web/static/js/vk/media.js::buildMediaCard(msg, att, group) -> ?HTMLElement (Ф7).

ПРИНИМАЕТ ВЛОЖЕНИЕ, А НЕ СООБЩЕНИЕ — ключевое отличие от телеграмного близнеца: у сообщения VK вложений до десяти. Группу карточек собирает renderAttachmentGroup(msg).

Раскладка карточки: head (глиф + имя файла или заголовок + вид), body (превью / иконка / волна голосового / кнопка воспроизведения / длительность / слот плеера), foot (метаданные + прогресс + статус + OPEN/SAVE).

Кнопки честно отражают возможности: у видео SAVE отключён (файла VK не отдаёт), OPEN ведёт на страницу плеера; у карточки other отключены обе. Волна голосового рисуется ПО waveform из DTO, а при его отсутствии — детерминированным шумом по (message_id, index), чтобы карточка не «дышала» при перерисовке.

Ключ реестра — <dialog>/<message>/<index>; по нему же ищется карточка для кадра download_progress. Превью ставится под IntersectionObserver, если canShowThumb(media); иначе сразу иконка, без запроса, который вернул бы 404.

Форматтеры (mediaMetaLine, mediaFileName, canShowThumb) получают СЫРОЙ DTO (att.raw): лексика snake_case одна на обе половины системы.

КАРТОЧКА ССЫЛКИ (Ф7, починка дефекта «не работают превью ссылок» — см. страницу vk-link-preview). Для kind=other с непустым MediaDto.link_url:
  * const link = webUrl(att.linkUrl) — строго http/https. Именно webUrl, а не safeUrl: safeUrl пропускает vk:, а схему адреса выбирал СОБЕСЕДНИК. Бэкенд уже отфильтровал (attach::http_url), здесь второй рубеж;
  * const domain = linkDomain(link) — подпись; полный адрес на карточку не едет, он ломает раскладку;
  * в foot первым элементом добавляется <a class="vk-media-link"> (hidden, если ссылки нет): textContent = домен, href = полный адрес, title = полный адрес, target=_blank, rel="noopener noreferrer". Атрибуты ставятся setAttribute, а не присваиванием свойств: у <a> свойство href нормализуется браузером относительно base;
  * OPEN включён (external = player || link), подпись подсказки «Перейти на <домен>»; openMedia получает ветку «!hasFile && ctl.linkUrl -> openExternal(ctl.linkUrl)». SAVE отключён с пояснением «это ссылка, а не файл»;
  * ctl.linkUrl хранится в контроллере, узел — в ctl.els.link.
Превью у такой карточки грузится обычным путём (observeThumb -> /api/vk/media/:peer/:mid/:idx?kind=thumb), потому что canShowThumb теперь просто верит серверному has_thumb.
