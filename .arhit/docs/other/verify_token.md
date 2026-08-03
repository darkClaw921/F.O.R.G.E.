# verify_token

plugins/vk/src/login.rs: проверка токена и перевод автомата LoginFlow в итоговое состояние.

Сигнатура: pub async fn verify_token<B: LoginBackend + ?Sized>(state: &Arc<VkState>, backend: &B, captcha: Option<&Captcha>) -> LoginFlow.

Порядок проверок ЗАФИКСИРОВАН и обратным быть не может:
1. LoginBackend::whoami (users.get) — дёшево и покажет код 5;
2. LoginBackend::probe_messages (messages.getConversations count=1) — покажет код 15.
Обратный порядок дал бы экран «нет права messages» на ПРОТУХШЕМ токене, и пользователь пошёл бы добывать токен с другими правами вместо того, чтобы обновить старый.

Перед первым сетевым вызовом состояние становится Checking — без него UI мигал бы «отключено» на каждом старте процесса (проверка запускается из spawn_workers). Без токена сеть не трогается вовсе: сразу NotConfigured.

Возвращает итоговое состояние, чтобы REST-хендлер отдал ровно его, не перечитывая состояние вторым запросом (между записью и чтением успел бы вклиниться другой переход).

Отображение отказов делает единственная функция apply_login_error: InvalidToken -> on_auth_invalidated, NoMessagesScope -> on_scope_lost, Captcha -> on_captcha (запоминает адрес картинки для прокси), Transport и Vk -> Disconnected с текстом. Исчерпывающий match: новый вариант LoginError невозможно забыть разложить по состояниям.

Trait LoginBackend существует ради тестируемости: матрица переходов (ok/5/14/15/транспорт) проверяется фейковым бэкендом без сети и без живого токена. Боевая реализация — HttpBackend поверх VkClient.
