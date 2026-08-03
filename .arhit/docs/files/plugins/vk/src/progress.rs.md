# plugins/vk/src/progress.rs

plugins/vk/src/progress.rs (Ф7) — отчёты о прогрессе передачи файлов.

Отдельный модуль, а не часть роута, ровно по одной причине: throttle обязан быть ОДИН на оба направления. Приём (download_progress, Ф7) и отправка (upload_progress, Ф8) иначе разъехались бы по частоте событий, и прогрессбар вёл бы себя по-разному в двух половинах одной переписки.

СОСТАВ:
  MIN_INTERVAL (100 мс), MIN_DELTA (256 КиБ).
  Throttle {should_emit(current, is_final), with_limits, last_bytes} — чистая логика без broadcast и DTO. Финальное событие проходит ВСЕГДА: иначе прогрессбар навсегда замирал бы на 97 %.
  DownloadReporter {new, for_key, report, finish} — вещает VkServerMsg::DownloadProgress.

ОТЛИЧИЕ ОТ ТЕЛЕГРАМНОГО БЛИЗНЕЦА: кадр несёт ДОПОЛНИТЕЛЬНОЕ поле index — порядковый номер вложения в сообщении. У Telegram вложение одно, у VK их до десяти, и без индекса прогресс второго фото прилетал бы в карточку первого. Каждый DownloadReporter держит СВОЙ Throttle, поэтому параллельные загрузки не глушат друг друга.

Половина отправки (ProgressReader поверх reqwest) приезжает в Ф8.
