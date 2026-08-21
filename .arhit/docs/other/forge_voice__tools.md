# forge_voice::tools

Модуль `plugins/voice/src/tools.rs` (220 строк) — ПОИСК ВНЕШНИХ ИНСТРУМЕНТОВ с отказом,
у которого есть код.

# ЗАЧЕМ

`ffmpeg`, `whisper-cli` и файл весов — три внешние зависимости, отсутствие каждой ломает
фичу по-своему. Без этого модуля отказ выглядел бы как `No such file or directory` из
недр запуска процесса; здесь он превращается в код (`no_ffmpeg`, `no_whisper`, `no_model`),
по которому фронт печатает человеку внятную причину и подсказку.

# КЛЮЧЕВЫЕ ЭЛЕМЕНТЫ

- `find_in_path(name)` — обход `PATH`, возвращает `Option<PathBuf>`.
- `resolve_ffmpeg(explicit)` — явный путь из конфига сильнее `PATH`;
  `FFMPEG_NAME = "ffmpeg"`.
- `resolve_whisper(explicit)` — перебирает `WHISPER_CANDIDATES = ["whisper-cli",
  "whisper-cpp", "main", "whisper"]`. Четыре имени потому, что whisper.cpp за версии
  переименовывал бинарь, и у людей в `PATH` встречается любое из них.
- `model_file_name(name)` / `model_path(models_dir, name)` — имя весов собирается как
  `MODEL_FILE_PREFIX + name + MODEL_FILE_SUFFIX` (`ggml-<name>.bin`), поэтому в конфиге
  человек пишет короткое `large-v3`, а не путь.
- `require_model(models_dir, name)` — существование весов, иначе `no_model`.

# ОГРАНИЧЕНИЯ

Модуль только НАХОДИТ; он не скачивает модель (скачивание — кнопка во вкладке настроек
встреч) и не проверяет работоспособность бинаря. То, что найденный whisper реально считает
на GPU, доказывает уже `whisper`-гард на живом stderr.
