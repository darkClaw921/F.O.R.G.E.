//! Служебные вызовы polza.ai: `GET /key`, `GET /balance`, `GET /models`.
//!
//! **Модуль переехал в [`forge_voice::cloud::service`]** вместе с клиентом:
//! методы объявлены на `CloudClient`, а inherent-impl можно писать только там,
//! где живёт тип. Здесь остался обратный `pub use` по прежнему пути —
//! `crate::cloud::service::KeyInfo` и остальные продолжают резолвиться, и ни
//! одно место вызова (`routes/config.rs`, `routes/status.rs`) не изменилось.
//!
//! Живые формы ответов и их разбор задокументированы в шапке модуля-оригинала:
//! суммы приходят СТРОКАМИ, цены лежат уровнем глубже, в `top_provider.pricing`.

pub use forge_voice::cloud::service::*;
