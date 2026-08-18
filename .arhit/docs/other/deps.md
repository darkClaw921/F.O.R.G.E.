# deps

deps.rs: пробы внешних бинарей плагина forge-meet (Ф5).

Живут в spawn_workers ОТДЕЛЬНОЙ ЗАДАЧЕЙ, а не в init: init синхронен и не имеет права запускать процессы (тест считает дочерние процессы), а первый в жизни запуск whisper-cli тратит 12,5 с на ggml_metal_library_init. Отсюда deps_probe_timeout_secs = 30 по умолчанию.

Что проверяется: ffmpeg и ffprobe (-version), whisper.cpp (перебор кандидатов), Chrome (наличие файла через forge_cdp::find_chrome), Metal, наличие моделей.

ПОРЯДОК КАНДИДАТОВ WHISPER и обязательная верификация: whisper-cli, whisper-cpp, main, whisper. Последний — ТОЛЬКО со строгой проверкой --version на строку whisper.cpp: на машине разработчика в PATH под именем whisper лежит Python openai-whisper с другим CLI, другими моделями и CPU по умолчанию. Принять его за свой значило бы уйти на CPU молча, ещё до всякого гарда. Тесты: the_python_whisper_in_path_is_rejected, the_candidate_order_prefers_whisper_cli_over_plain_whisper.

ФЛАГИ ПРОВЕРЯЮТСЯ ПРОГРАММНО: WhisperFlags::parse_help разбирает вывод --help (он идёт в STDERR, не в stdout) по длинным именам (--output-json-full и т.п.). Отсутствующий флаг выключается: передай мы -ojf сборке, которая его не знает, whisper упал бы на разборе аргументов.

gpu_metal берётся из той же справки по строке ggml_metal_device_init: — это признак НАЛИЧИЯ устройства, а не доказательство расчёта на нём. Доказательство — дело гарда (см. страницу meet-metal-guard).

POST /api/meet/deps/recheck перепроверяет окружение без перезапуска devforge и вещает кадр deps. Зовётся также после успешного скачивания модели: кнопка «Расшифровать» обязана ожить сама.
