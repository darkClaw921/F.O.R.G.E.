# app.js::formatGlobalId

Phase 5 — Собирает глобальный id из origin + local. Для origin='local' (или пустого) возвращает только local id без префикса. В remote-mode используется когда id уходит в localStorage / history.pushState / клипбоард, чтобы later parseGlobalId восстановил origin.
