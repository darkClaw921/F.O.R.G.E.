# Конвейер отправки файлов в VK (Фаза 8)

## Главное решение: конвейер живёт внутри POST /messages

У Telegram отправка файла — один шаг (upload_stream -> Uploaded -> sendMedia). У VK их четыре:

    (1) photos.getMessagesUploadServer(peer_id)                     [ТРОТТЛЕННЫЙ]
        docs.getMessagesUploadServer(peer_id, type=doc|audio_message)
    (2) POST <upload_url> multipart                                 [НЕ троттлится]
    (3) photos.saveMessagesPhoto | docs.save                        [ТРОТТЛЕННЫЙ]
    (4) messages.send(attachment="photo{owner}_{id}")               [ТРОТТЛЕННЫЙ]

ПОЧЕМУ ЭТО НЕ ВНУТРИ POST /api/vk/upload. Адрес upload-сервера VK выдаёт ПОД КОНКРЕТНЫЙ peer_id. На шаге «пользователь прикрепил файл» адресат ещё неизвестен: файл можно выбрать и передумать, куда его слать, или переключить беседу. Спросить адрес заранее не у кого, а спросить «на всякий случай для текущей беседы» значит потратить троттленный запрос из бюджета 3 rps на догадку, которая часто окажется неверной.

Отсюда разделение:

- POST /api/vk/upload кладёт байты на НАШ диск (<cache_dir>/uploads/<uuid>.part) и больше НИЧЕГО не делает — ни одного обращения к VK (тест upload_never_touches_vk смотрит на нетронутое ConnHealth, тест staging_touches_neither_vk_nor_the_progress_channel — на пустой журнал вызовов стенда);
- POST /api/vk/dialogs/:id/messages выполняет все четыре шага подряд, уже зная peer_id.

## Три следствия, которые обязаны быть на виду

1. upload_id НЕ соответствует attachment-строке VK и никогда ей не станет. Это наш идентификатор staging-файла; attachment-строка рождается на шаге (3) и живёт ровно до messages.send в том же запросе.
2. Кадр upload_progress приходит ВО ВРЕМЯ POST /messages, а не во время POST /upload. Это единственное поведенческое отличие, видимое фронту; форма кадра при этом телеграмная, и фронт обязан лишь продолжать слушать его после нажатия «отправить» (js/vk/upload.js, этап «VK %»).
3. Staging — полуфабрикат, а не кэш: первая же отправка его расходует.

## Альбомы заложены сразу

VK принимает до пяти фотографий ОДНИМ multipart-POST (поля file1..file5), сохраняет их одним photos.saveMessagesPhoto и отправляет одним messages.send со строкой "photo1_2,photo3_4,...". Пятёрка по одному стоила бы 15 троттленных запросов и пять секунд при лимите 3 rps; альбомом — те же три и около секунды. Поэтому POST /messages принимает upload_ids: Vec<String> (upload_id остался односоставным синонимом ради формы Telegram и диффабельности js/vk/upload.js с телеграмным близнецом). Не заложив это в контракт сразу, потом пришлось бы ломать фронт.

Порядок вложений в строке attachment — порядок пользователя: документы уезжают по одному между альбомами фотографий, но слоты сохраняются по исходным индексам.

## Расход upload_id безвозвратен

take_uploads потребляет записи реестра ДО заливки. Повторный POST /messages с тем же upload_id получает 409 invalid_state. Это осознанный размен: двойной клик по «Отправить» и автоповтор фронта случаются гораздо чаще неудачной заливки, а цена ошибок разная — лишнее сообщение у собеседника не отзывается, файл же всегда можно залить заново. Staging после попытки удаляется при любом исходе.

## Идемпотентность отправки

random_id генерируется ОДИН РАЗ на логическую отправку, а транспорт при повторе шлёт ТЕ ЖЕ параметры (VkClient::call_value). VK дедуплицирует messages.send по random_id, поэтому обрыв на ответе не превращается в дубль сообщения и не заставляет перезаливать файл (тест a_transport_retry_of_a_file_send_reuses_the_random_id: два messages.send, один multipart-POST, один getMessagesUploadServer).

## Почему stream_with_length обязателен

Part::stream_with_length(body, size) проставляет Content-Length части. С обычным stream() reqwest уходит в chunked — который upload-сервер VK не принимает — и буферизует файл целиком: отправка полугигабайтного видео уехала бы в RSS процесса. Мутация «заменить на stream()» краснит upload_multipart_streams_with_a_declared_length и an_album_of_five_photos_still_costs_three_throttled_calls.

## Порядок проверок в POST /api/vk/upload

авторизация (409) -> Content-Length (411) -> лимит (413) -> X-Forge-File-Name (400) -> стриминг на диск. Каждая ступень отвечает ДО чтения тела: сырой TCP-тест upload_refuses_before_reading_the_body посылает заголовок с обещанными 64 МиБ и десять байт тела — ответ обязан прийти, а не подвиснуть в ожидании гигабайта.

## Хост

tmux-web/src/auth.rs: RAW_UPLOAD_PATHS = ["/api/telegram/upload", "/api/vk/upload"], сравнение ТОЧНОЕ по полному пути. Префиксное исключение (starts_with("/api/vk/")) отвергнуто: под него автоматически попал бы любой будущий мутирующий роут плагина, молча потеряв защиту по Content-Type. Тесты other_vk_routes_still_require_json и lookalike_upload_paths_are_not_exempt краснеют ровно на этой мутации.

## Карта файлов

- plugins/vk/src/routes/upload.rs — staging и весь конвейер;
- plugins/vk/src/routes/dialogs.rs::post_message — точка вызова (collect_upload_ids -> take_uploads -> build_attachment -> messages.send);
- plugins/vk/src/api/mod.rs — UploadPart, upload_multipart_parts;
- plugins/vk/src/progress.rs — ProgressReader, UploadReporter;
- plugins/vk/src/state.rs — UploadEntry, реестр uploads, take_upload, TTL;
- tmux-web/static/js/vk/upload.js — фронт; tmux-web/tests/frontend/vk_upload.test.mjs — его тест;
- plugins/vk/tests/vk_stub.rs — стенд (13 сценариев Ф8, включая сырой TCP).