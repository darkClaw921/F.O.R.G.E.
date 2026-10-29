# settings.modal.analyticsLifecycle

Модалка сохраняет результат renderAnalyticsTab в analyticsTabState.view. Закрытие overlay вызывает view.dispose до удаления DOM. Уход showTab с analytics также вызывает dispose, обнуляет view и loaded; возвращение создаёт новый renderer и возобновляет ровно один опрос. getAgentLive приходит инъекцией из agentsApi.live, поэтому renderer по-прежнему не импортирует browser/API состояние напрямую. Другие вкладки и их загрузка не изменены.
