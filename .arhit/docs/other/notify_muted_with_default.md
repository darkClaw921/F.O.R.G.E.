# notify_muted_with_default

plugins/telegram/src/dto.rs — notify_muted с наследованием глобального дефолта категории.

Сигнатура: notify_muted_with_default(settings: &tl::types::PeerNotifySettings, now: i64,
default_muted: bool) -> bool.

ПРИОРИТЕТЫ (порядок обязателен, менять нельзя):
1. явный silent == Some(true) ⇒ заглушён;
2. явный mute_until > now ⇒ заглушён;
3. явный mute_until в прошлом или 0 ⇒ НЕ заглушён — точечный unmute перебивает даже
   «заглушить все чаты» (иначе диалог, размьюченный вручную, продолжал бы молчать);
4. настройки ПУСТЫ ЦЕЛИКОМ (silent и mute_until — оба None) ⇒ берётся default_muted.

Проверка «пусто целиком» намеренно смотрит на ОБА поля. Сервер шлёт объект с выставленным
silent и унаследованным mute_until — такой peer по контракту считается настроенным ЯВНО, и
глобальный дефолт к нему не применяется. Если бы условие смотрело только на silent.is_none(),
диалоги с частичными настройками ошибочно уезжали бы в дефолт категории.

СВЯЗЬ С notify_muted: старая функция сохранена без изменений (эквивалент
notify_muted_with_default(.., false)), чтобы существующие вызовы и тесты не поехали. Все новые
пути (мапперы диалогов, updates::notify_outcome) зовут именно версию с дефолтом.

Зовут: dialog_muted_with_defaults_for_kind, from_raw_dialog_with_defaults,
from_grammers_dialog_with_defaults, updates::notify_outcome.
