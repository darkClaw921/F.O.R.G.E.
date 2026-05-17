# tmux-web/static/js/core/api.js

Phase 1 update. apiFetch теперь импортирует isRemoteMode из remote/healthz.js (вместо inline state.remoteMode===true). 1:1 функционально, но через явный публичный API healthz-модуля.
