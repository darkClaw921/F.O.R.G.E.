# from_grammers_dialog_with_defaults

plugins/telegram/src/dto.rs — сборка DialogDto из клиентского grammers Dialog с наследованием
глобальных дефолтов уведомлений.

Сигнатура: from_grammers_dialog_with_defaults(dialog: &grammers_client::peer::Dialog, now: i64,
defaults: NotifyDefaults) -> DialogDto.

Отличие от старой from_grammers_dialog (сохранена без изменений, эквивалент defaults=Default):
muted разрешается с подстановкой дефолта категории (D1), bot/contact заполняются через
peer_bot_contact (D3).

Используется в routes/dialogs.rs на пути iter_dialogs (обычный листинг GET /api/telegram/dialogs).
После сборки списка роут обязан позвать TelegramState::remember_dialogs — именно этот засев
кэшей делает WS-кадр new_message способным честно ответить «заглушён ли диалог» для чатов,
которых фронт никогда не показывал.
