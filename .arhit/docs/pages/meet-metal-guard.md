Metal-гард: структурное исполнение правила проекта «локальные модели только на GPU».

ДВА независимых механизма, оба — тесты, а не комментарии:
1) build_args (чистая функция) не содержит -ng/--no-gpu НИ ПРИ КАКИХ настройках. Тест перебирает 4096 комбинаций флагов, языков, ширины луча и наличия VAD-модели. Приём взят из теста приватности в claude-runner.
2) stderr прогона парсится на ТОЧНЫЙ ПРЕФИКС строки ggml_metal_init: (функция metal_confirmed). Не нашли — процесс убивается с metal_unavailable.

ПОЧЕМУ ПРЕФИКС, А НЕ ПОДСТРОКА. Проба Ф0 и живой тест the_installed_whisper_still_behaves_as_the_guard_assumes: строки ggml_metal_device_init:, ggml_metal_library_init:, ggml_metal_rsets_init: печатаются ВСЕГДА — в том числе с -ng и даже на --help. Гард stderr.contains("ggml_metal") пропустил бы CPU-прогон, то есть нарушил бы правило молча: транскрипт получился бы тот же, только через полтора часа вместо четырёх минут. Поле systeminfo в JSON не годится тоже — у GPU- и CPU-прогона оно побайтово одинаково.

ТРИ пути срабатывания гарда:
- строка-маркер процессора (whisper_backend_init_gpu: no GPU found, whisper_backend_init: using BLAS backend) — убийство немедленно;
- окно ожидания metal_grace_secs (60 с по умолчанию: первый в жизни запуск тратит 12,5 с на ggml_metal_library_init) — убийство по таймеру;
- проверка ПОСЛЕ выхода процесса — быстрый CPU-прогон успевает закончиться сам, и его JSON удаляется.

ЖИВОЕ ИЗМЕРЕНИЕ (15.08.2026, whisper-cpp 1.9.2, M1 Max, 1 с речи): GPU encode 846 мс, CPU (-ng) encode 10 699 мс — разрыв 12,6x. metal_confirmed на настоящем выводе: GPU true, CPU false.

Разница с пробой deps.rs: проба спрашивает «есть ли Metal-устройство» и смотрит на ggml_metal_device_init: в выводе --help; гард спрашивает «этот прогон реально пошёл на GPU» и требует ggml_metal_init:. Совпади они — правило держалось бы на уговоре. На различие есть тест the_probe_and_the_guard_look_at_different_lines.