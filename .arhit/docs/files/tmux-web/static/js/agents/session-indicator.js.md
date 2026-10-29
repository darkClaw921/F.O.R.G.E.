# tmux-web/static/js/agents/session-indicator.js

Чистый formatter generatingTooltip(session) для синей звезды вкладки. Читает working_agent_count/agent_providers и generating_since_secs из совместимого /api/sessions DTO. Показывает провайдеров, число основных задач и возраст самой ранней задачи; при отсутствии новых полей старого remote использует один неизвестный агент. Неположительный/NaN возраст не отображается как число. Formatter не анализирует терминал и не управляет orange attention или pulse CSS.
