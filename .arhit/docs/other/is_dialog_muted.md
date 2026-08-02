# is_dialog_muted

plugins/telegram/src/state.rs — TelegramState::is_dialog_muted(&self, dialog_id: i64) -> Option<bool>.

Возвращает УЖЕ РАЗРЕШЁННЫЙ (с наследованием глобальных дефолтов) признак «заглушён» из кэша
notify_overrides.

КЛЮЧЕВОЕ: None означает «НЕ ЗНАЕМ», а НЕ «не заглушён». Вызывающий обязан подставить
NotifyDefaults::for_kind(kind) — см. updates::resolve_dialog_flags. Если бы функция возвращала
голый bool с false на промахе, диалог из глобально заглушённой категории, которого нет в кэше,
снова начал бы звенеть — то есть вернулся бы ровно тот дефект, из-за которого всё это делалось.

Пара: set_dialog_muted (точечная запись, зовётся из apply_effects по updateNotifySettings) и
remember_dialogs (массовый засев из листинга).
