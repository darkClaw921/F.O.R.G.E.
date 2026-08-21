# waveform-core

tmux-web/static/js/messenger/waveform-core.js — ОДИН рисовальщик звуковой дорожки голосового на три панели (VK, Telegram, Bitrix24). Появился в Ф2 фичи голосовых сообщений.

ЗАЧЕМ. До Ф2 ветку настоящей огибающей имел только VK (js/vk/media.js buildWave), Telegram всегда рисовал детерминированный шум, а Bitrix не имел волны вовсе. Копия ветки в третьей панели означала бы три расходящихся вида одной карточки, поэтому логика вынесена сюда, а панели передают только своё имя класса и свою соль.

API:
- WAVE_BARS = 40, WAVE_MIN_PX = 8, WAVE_RANGE_PX = 24 — геометрия столбиков, общая для .vk-media-wave / .tg-media-wave / .b24-media-wave (CSS повторяет её один в один).
- noiseSeed(messageId, index = null) — соль заглушки. index пропускают панели, у которых вложение в сообщении одно (Telegram): подмешивание нуля изменило бы рисунок всех уже нарисованных заглушек.
- realHeights(waveform) — высоты настоящей волны. Нормализация ПО МАКСИМУМУ САМОГО МАССИВА (иначе тихая запись была бы полоской у нижнего края), прореживание шагом ceil(len / WAVE_BARS): массив бывает до 200 отсчётов (forge_voice::waveform::MAX_WAVEFORM), столбиков сорок.
- noiseHeights(seed) — xorshift32-заглушка ровно на WAVE_BARS столбиков.
- waveHeights(waveform, seed) — настоящая волна, если она есть, иначе заглушка. Чистая функция, на ней держатся тесты.
- buildWave({waveform, seed, className, doc}) — готовый узел с aria-hidden. doc параметром, потому что разметку Bitrix строит фейковый документ фронт-тестов.
- hasRealWaveform(media).

ОТКУДА БЕРЁТСЯ media.waveform: VK — из ответа API; Telegram — из DocumentAttributeAudio.waveform (5 бит на отсчёт, распаковка forge_voice::waveform::from_telegram); Bitrix и телеграмные аудиофайлы без атрибута — считает бэкенд по скачанному файлу (forge_voice::waveform::compute) и кэширует сайдкаром рядом с байтами. Пока настоящей огибающей нет, остаётся заглушка.

Потребители: js/vk/media.js (buildWave), js/telegram/media.js (карточка voice/audio), js/bitrix/attachments.js (buildMediaCard).
Тесты: tmux-web/tests/frontend/messenger_waveform_core.test.mjs (+ страж единственности), telegram_voice_wave.test.mjs, vk_media.test.mjs, bitrix_media.test.mjs.
