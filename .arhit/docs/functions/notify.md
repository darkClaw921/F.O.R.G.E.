# notify

tmux-web/static/js/echo/notifications.js: общий toast-компонент. notify({level:'info'|'warn'|'error', title, body, ttl, glass}). glass:true добавляет класс .echo-toast-glass — стиль «жидкое стекло» (полупрозрачный backdrop-filter blur + saturate, внутренняя подсветка), css/echo-notifications.css. Контейнер #echo-toasts — глобальный (fixed справа сверху, z-index 2000), виден независимо от активной вкладки. Очередь до MAX_TOASTS, slide-in справа.
