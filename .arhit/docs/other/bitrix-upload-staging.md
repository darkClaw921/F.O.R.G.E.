# bitrix-upload-staging

Реестр загрузок и staging-файлы (Ф9) — plugins/bitrix/src/routes/upload.rs +
plugins/bitrix/src/state.rs.

ЧТО ЭТО
Принятый от браузера файл не отправляется сразу: он ложится в
<cache_dir>/uploads/<upload_id>.part, а запись о нём — в реестр
B24State::uploads (HashMap<String, UploadEntry> под Mutex). Клиенту уезжает
upload_id, и только следующий запрос (POST /dialogs/:id/messages с полем
upload_id) превращает полуфабрикат в сообщение.

UploadEntry: path, file_name, size, mime, created_at (unix-секунды).

ИМЯ ФАЙЛА НА ДИСКЕ — ТОЛЬКО UUID
staging_path(cfg, upload_id) = <cache_dir>/uploads/<uuid>.part. Ни одного байта
пользовательского ввода в пути нет, поэтому path traversal через заголовок
X-Forge-File-Name невозможен ПО ПОСТРОЕНИЮ, а не по качеству санитизации.
Санитизация имени всё равно делается — но ради того, ЧТО УВИДИТ ПОЛУЧАТЕЛЬ в
чате, а не ради файловой системы.

ТРИ СОБЫТИЯ, ПОСЛЕ КОТОРЫХ .part ИСЧЕЗАЕТ
  1. отправка — send_staged_file потребляет запись и убирает файл В ЛЮБОМ
     ИСХОДЕ, включая ошибку портала: повторять его запрещено (Р9), а место он
     занимает;
  2. TTL — UPLOAD_TTL_SECS (30 минут): файл выбрали и передумали. Убирает
     uploads_gc_loop (интервал UPLOAD_GC_INTERVAL = 5 минут) и ленивая
     подчистка evict_stale на каждой новой загрузке — реестр не растёт даже
     если воркер почему-то не запущен (урезанная сборка, тест);
  3. рестарт процесса — cleanup_uploads_dir чистит каталог ЦЕЛИКОМ первым же
     прогоном GC-воркера. Реестр живёт только в памяти, поэтому после
     перезапуска каждый .part — сирота: сослаться на него уже некому.

Плюс logout: clear_uploads(state) вызывается из routes::login::post_logout.
Невыбранный файл — такие же данные аккаунта, как имена сотрудников и счётчики;
пережив отвязку, он уехал бы в чат СЛЕДУЮЩЕГО портала первой же отправкой.

ПОЧЕМУ take_upload ПОТРЕБЛЯЮЩИЙ
B24State::take_upload убирает запись из реестра ЕЩЁ ДО заливки. Это осознанный
размен: повторный POST /messages с тем же upload_id обязан получить
409 invalid_state, а не отправить файл второй раз. Двойной клик по «Отправить»
и автоповтор фронта случаются куда чаще, чем неудачная заливка, и цена ошибки у
них разная: лишний файл в чате у собеседника не отзывается, а залить заново
можно всегда. Защита стоит на уровне МЕХАНИКИ, а не на уровне UI — фронт может
быть закэширован PWA в любой версии.

Идентификатор в текст ошибки НЕ подставляется: ответ не должен работать
рефлектором чужого ввода.

ПОЧЕМУ НЕ media_cache::begin_staging/commit
Те кладут результат в media/ под ключом кэша и рассчитаны на повторное чтение;
здесь результат — не кэш, а полуфабрикат, расходуемый первой же отправкой.
Общий у них только приём: guard, снимающий недописанный файл на Drop
(StagingGuard) — веток выхода из хендлера больше, чем кажется (обрыв
соединения дропает future целиком).

ТЕСТЫ (без сна на полчаса — время инъецируется)
  take_upload_consumes_the_entry — второй вызов даёт 409;
  stale_uploads_are_evicted_with_their_files — evict_stale_at(ttl, now + ttl + 60);
  a_fresh_upload_survives_the_gc — TTL не режет по живому;
  a_process_start_removes_orphaned_part_files;
  logout_clears_the_registry_and_the_staging_files;
  a_short_body_leaves_no_staging_file — guard снимает файл при отказе;
  tests/upload.rs: a_consumed_upload_id_cannot_be_used_twice,
  a_failed_upload_still_discards_the_staging_file.
