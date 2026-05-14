# app.js::parseGlobalId

Phase 5 — Парсит глобальный id формата 'origin::local-id' → { origin, id }. Если в строке нет '::', возвращает { origin: 'local', id }. Используется когда фронт получает id извне (URL, history) и нужно распаковать в origin + бэкенд-id. В legacy-режиме id всегда простой, parseGlobalId возвращает origin='local'.
