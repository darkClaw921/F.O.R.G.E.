# app.js::apiFetch

Phase 5 — Centralized fetch helper для API-вызовов, которые могут уходить на remote (sessions/projects/tasks/todos). Сигнатура: apiFetch(path, init, origin). Если isRemoteMode() и origin !== 'local' → добавляет ?server=<origin> к path (через withServerParam). Не используется для /healthz, /api/themes, /api/remote-servers — они только local. В legacy-режиме игнорирует origin.
