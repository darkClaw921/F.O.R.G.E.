# openEditModal

JS modal-builder для редактирования issue. Принимает текущий issue object. Заполняет поля text/textarea/selects (priority, status, type) текущими значениями + показывает id в .modal-id. Кнопки: Save (PATCH с diff-only payload — шлёт только изменённые поля), Cancel, Close (с window.prompt reason → DELETE) — для не-closed задач, Reopen — для closed. Type в API не маппится на текущий момент, поэтому изменение type из UI пока не пробрасывается на сервер.
