//! CRUD-репозитории над таблицами Echo.
//!
//! Каждый sub-модуль соответствует одной логической группе таблиц и
//! экспортирует доменный struct + async-функции `create / list / get /
//! update / delete`. Repo-слой ничего не знает про axum-handler'ы — он
//! принимает `&Db`.

pub mod app_settings;
pub mod autonomous;
pub mod chats;
pub mod daily_reports;
pub mod memories;
pub mod messages;
pub mod stats;
