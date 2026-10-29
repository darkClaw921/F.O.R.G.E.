# settings.analytics.agentsVisibilityChanged

Обслуживает видимость браузерной вкладки для компактного live showcase настроек. При document.hidden снимает agent timeout; при возвращении вызывает loadAgents только для живого renderer без уже запланированного таймера. dispose удаляет listener. Это не таймер настроечной формы: источники и тарифы остаются отдельным ленивым draft UI.
