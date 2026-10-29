# tmux-web/static/js/pwa/bootstrap.js::routeView

Маршрутизация deep link сохраняет белый список представлений. Для view=analytics добавлен необязательный analyticsMode: точное значение claude открывает прежнюю аналитику работы с Claude, остальные значения открывают режим Агенты. Значение не используется как путь динамического импорта. Поэтому legacy кнопка с /?view=analytics&analyticsMode=claude продолжает открывать прежние показатели, а обычный view=analytics открывает новый default. PWA opt-in и остальная маршрутизация не меняются.
