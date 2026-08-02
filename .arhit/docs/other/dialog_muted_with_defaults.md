# dialog_muted_with_defaults

plugins/telegram/src/dto.rs — dialog_muted (для сырого tl::enums::Dialog) с наследованием
глобальных дефолтов уведомлений.

Две формы:
- dialog_muted_with_defaults(raw: &tl::enums::Dialog, now: i64, defaults: NotifyDefaults) -> bool —
  категория выводится по тегу PeerId через NotifyDefaults::for_peer_id. ОГРАНИЧЕНИЕ: мегагруппа
  по тегу id неотличима от broadcast-канала и получит дефолт broadcasts. Промах возможен ТОЛЬКО
  у диалога с полностью унаследованными настройками при РАЗНЫХ дефолтах chats/broadcasts.
- dialog_muted_with_defaults_for_kind(raw, now, defaults, kind: &str) -> bool — категория задана
  явно ("user"/"group"/"channel"). Это правильная форма везде, где есть RawPeerIndex: там
  мегагруппа честно помечена как "group".

У Dialog::Folder (строка «Архив» в основном списке) настроек уведомлений нет вовсе — он никогда
не «заглушён», обе формы возвращают false.

Старая dialog_muted сохранена без изменений (эквивалент с NotifyDefaults::default()).

Зовут: from_raw_dialog_with_defaults (через _for_kind, kind берётся из индекса peer'ов),
routes/dialogs.rs::folder_dialogs.
