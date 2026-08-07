# tmux-web/static/js/bitrix/attachments.js

Разметка вложений и карточек ATTACH панели «Битрикс24».

С Фазы 8 buildMediaCard(media, doc, msg) строит ПОЛНУЮ карточку: шапка (глиф, имя, вид), тело (превью-<img> только у kind==='image' с файлом, иконка, слот плеера), подвал (метаданные, полоса прогресса, статус, кнопки OPEN/SAVE). ЧИСТАЯ РАЗМЕТКА: ни addEventListener, ни fetch, ни objectURL — всё это живёт в media.js, который находит узлы по классам. Разделение позволяет проверять карточку в голом Node с фейковым документом (bitrix_bbcode.test.mjs), где нет ни addEventListener, ни URL.

Атрибут src при построении НЕ ставится никогда: байты приезжают лениво и только через прокси. <img> заводится лишь там, где его есть чем наполнить — пустой <img> у документа был бы обещанием, которое нечем исполнить (отдельного маленького файла у портала нет: im.v2.File.download отдаёт оригинал).

dataset карточки: kind, index, fileId, dialogId, msgId — это адрес прокси и ключ кадра download_progress.

buildMediaList(list, doc, msg) — контейнер .b24-line-media. buildCards(list, doc) — карточки params.ATTACH (блоки MESSAGE/LINK/GRID). Два правила, которые легко потерять: только createElement/textContent (innerHTML запрещён — содержимое пишут боты портала) и адрес ссылки НЕ становится href (схему уже проверил attach.rs::http_url, но рубежа должно быть два, а href с чужим адресом это ещё и утечка referrer'а).
