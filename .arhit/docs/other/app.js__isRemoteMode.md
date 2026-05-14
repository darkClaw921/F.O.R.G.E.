# app.js::isRemoteMode

Phase 5 — Guard-функция. Возвращает true, если frontend должен рендерить новый UI (origin-табы, Settings → Remote servers tab, кнопка Add remote, ?server= в API/WS-вызовах, глобальные id формата ::). При false поведение фронта побитово совпадает с legacy. Используется в renderSidebar, openSettingsModal, apiFetch и connectXxxWs.
