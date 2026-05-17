# tmux-web/static/js/echo/notifications.js

General-purpose toast уведомления (доступны из любого модуля, не только Echo). notify({level,title,body,ttl=5000}) показывает toast в #echo-toasts, queue cap=5 (вытесняем самый старый), CSS classes echo-toast-show/echo-toast-hide для slide-in/fade-out (см. echo-notifications.css). clearToasts() для очистки. notifyFromServerMsg(msg) маппит ServerMsg::Notification → notify.
