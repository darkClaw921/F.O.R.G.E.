# js/vk/attachments.js

tmux-web/static/js/vk/attachments.js — нормализация вложений и пересланных (Ф4.7, НОВЫЙ; аналога в js/telegram/* нет). Покрыт tests/frontend/vk_attachments.test.mjs (105 проверок).

API: normalizeAttachments(msg, {max}) -> модель карточек с сохранением ПОРЯДКА И ИНДЕКСОВ (index входит в ключ медиа-кэша и в маршрут /api/vk/media/:peer/:mid/:idx — фильтрация «ненужных» видов показала бы не тот файл); attachmentKind(att) (словарь MediaDto + сырые имена VK для кадров WS, неизвестный -> unsupported); primaryAttachment; attachmentsSummary (глифы + русское склонение, cap MAX_INLINE_CARDS=6); hasRenderableMedia; flattenForwards(msg, maxDepth=MAX_FWD_DEPTH=2) -> {rows, truncatedCount}; forwardsTail; forwardPreview.

ИНВАРИАНТ: url ВСЕГДА null. Прямые ссылки VK подписанные и протухающие, а запрос браузера с Referer devforge на серверы VK неуместен для панели с личной перепиской. Поле объявлено явно, чтобы правило нельзя было «забыть»; бэкенд прямых ссылок и не отдаёт — это второй рубеж. Исключение одно: playerUrl (страница-плеер видео).

ГЛУБИНА ПЕРЕСЫЛОК ограничена 2 — меньше серверных 3 намеренно: сервер отдаёт уровень про запас, чтобы «…ещё N пересланных» считалось по настоящим данным. У VK цепочка пересылок не ограничена ничем, и наивная рекурсия рисует экран из одного сообщения. Функция принимает и плоскую форму сервера (msg.fwd с depth/omitted), и сырую рекурсивную (fwd_messages).
