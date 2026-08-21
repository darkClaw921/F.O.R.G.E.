# Ф2 фичи голосовых: настоящая звуковая дорожка вместо заглушки

Эпик forge-cvoice-f2-6iahf. До фазы реальную огибающую показывал ровно один мессенджер из трёх (VK), остальные рисовали детерминированный шум, одинаковый у односложного «да» и у полутораминутного монолога.

## Три источника огибающей — одно поле DTO

| мессенджер | откуда берётся |
|---|---|
| VK | приходит в ответе API (было и раньше) |
| Telegram, голосовые | DocumentAttributeAudio.waveform, упаковка 5 бит на отсчёт |
| Telegram, обычное аудио | считает бэкенд по скачанному файлу |
| Bitrix24 | считает бэкенд: портал огибающую не отдаёт вовсе |

Всё складывается в одно и то же поле MediaDto.waveform в одной шкале — 0..=255, не длиннее 200 столбиков (forge_voice::waveform::{WAVEFORM_MAX_VALUE, MAX_WAVEFORM}), и рисуется одним кодом фронта.

## Что появилось

Ядро (plugins/voice/src/waveform.rs):
- unpack_5bit / fit / from_telegram — распаковка Telegram и приведение чужой шкалы к общей;
- WaveformCache, sidecar_for, read_cached, write_cached, is_cached, ensure — слой кэша посчитанной огибающей.

Плагины (plugins/{bitrix,telegram}/src/voice.rs): is_audio, cached_waveform, ensure_waveform, spawn_waveform, attach_waveforms. Вызовы: spawn_waveform сразу после media_cache::commit, attach_waveforms — на собранную страницу истории.

Фронт: tmux-web/static/js/messenger/waveform-core.js — один рисовальщик на три панели; подключён из vk/media.js, telegram/media.js, bitrix/attachments.js. Копий ветки в панелях не осталось (страж в тесте). Стили .b24-media-wave добавлены и повторяют .vk-media-wave один в один.

## Решения, которые не надо пересматривать

1. **Кэш огибающей — отдельный сайдкар <stem>.wave.json, а не поле в .meta.json.** Мету пишет commit РАНЬШЕ расчёта; дописывание поля из фоновой задачи было бы read-modify-write чужого файла в гонке с get.
2. **Расчёт в фоне, а не в строке роута.** Человек в этот момент ждёт байты вложения; отсутствие огибающей — рабочее состояние карточки, а не ошибка.
3. **Атрибут Telegram всегда побеждает расчёт.** Он бесплатен и точен.
4. **Усреднение по бакетам, а не прореживание каждый N-й.** Прореживание теряет ровно пики — ту форму, ради которой волна и рисуется.
5. **Заглушка остаётся** — но только пока настоящей огибающей нет.

## Деградация

Нет ffmpeg — нет волны, и больше ничего: лента, кэш и скачивание работают как работали, в журнале одна внятная строка (тесты a_missing_ffmpeg_costs_only_the_waveform). Битый и не-аудио файл commit не роняют.

## Проверки

cargo test --workspace зелёный; фронт-тесты messenger_waveform_core.test.mjs (50 проверок, страж единственности) и telegram_voice_wave.test.mjs (17 проверок на реальном buildMediaCard) плюс прежние vk_media / bitrix_media / telegram_media без правок. Живая проба боевого пути на настоящем ffmpeg и настоящем ogg/opus — тест #[ignore] the_real_ffmpeg_reads_a_real_opus_voice_message в plugins/voice/src/waveform.rs.