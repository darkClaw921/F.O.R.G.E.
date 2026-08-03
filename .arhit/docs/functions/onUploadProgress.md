# onUploadProgress

Обработчик кадра WS upload_progress в панели VK (tmux-web/static/js/vk/upload.js). Находит карточку по upload_id и двигает вторую ногу прогресса («VK %»). ВАЖНО: у VK кадры приходят во время POST /api/vk/dialogs/:id/messages, а не во время POST /api/vk/upload — трёхшаговый конвейер живёт внутри отправки сообщения, поэтому слушать их надо и ПОСЛЕ нажатия «отправить». Кадры вещаются всем вкладкам без фильтрации, поэтому чужие и уже завершённые upload_id молча игнорируются. Подключён в js/vk/main.js::buildHandlers.
