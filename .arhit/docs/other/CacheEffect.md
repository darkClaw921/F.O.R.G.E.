# CacheEffect

plugins/telegram/src/updates.rs — побочные эффекты разбора апдейта, вынесенные в данные.

enum CacheEffect (Debug, Clone, PartialEq, Eq):
- Defaults(NotifyDefaults) — записать новые глобальные дефолты;
- DialogMuted { dialog_id: i64, muted: bool } — записать разрешённый muted конкретного диалога.

ЗАЧЕМ: разбор апдейта остаётся ЧИСТОЙ функцией (тестируется без сети и без состояния), а запись
в RwLock-кэши делает отдельная async apply_effects(state, &[CacheEffect]).

ИНВАРИАНТ ПОРЯДКА: apply_effects пишет кэши ДО broadcast кадра. Иначе клиент, который в ответ на
кадр дёрнет REST, успел бы прочитать УСТАРЕВШЕЕ состояние — гонка, которую крайне трудно поймать
в проде.
