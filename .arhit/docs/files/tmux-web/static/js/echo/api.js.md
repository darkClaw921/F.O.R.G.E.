# tmux-web/static/js/echo/api.js

Echo plugin REST API client. Тонкие async wrappers вокруг apiFetch из core/api.js для /api/echo/*: conversations CRUD (list/create/delete/messages), memories CRUD (list/create/patch/delete) + regenerate (POST /api/echo/memories/regenerate с scope/project_id/day), autonomous-tasks (list/create/patch/delete/run-now/runs), stats, cancelRun. Helper call() парсит JSON и бросает Error со status+message при !res.ok.
