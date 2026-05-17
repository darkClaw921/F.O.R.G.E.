# tmux-web/static/js/remote/healthz.js

Phase 1. loadHealthz() — GET /healthz (без auth), пишет state.remoteMode/serverVersion/healthzLoaded. isRemoteMode() — true iff state.remoteMode === true. Используется как feature-toggle во всех модулях (api/sidebar/ws/...). При сбое — remoteMode=false (legacy fallback).
