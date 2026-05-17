# tmux-web/static/js/echo/action-buttons.js

Echo action buttons UI. renderActionButtons(messageEl, descriptors, invokeCb) — рендерит кнопки под assistant-сообщением. Prompt-actions вызывают invokeCb сразу; system-actions показывают showConfirmationModal с params JSON pretty-print, кнопками Cancel/Run, Esc→cancel, Enter→confirm. Возвращает Promise<bool> для async-await.
