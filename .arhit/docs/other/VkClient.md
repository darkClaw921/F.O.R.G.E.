# VkClient

plugins/vk/src/api/mod.rs. HTTP-клиент api.vk.com — единственная точка выхода плагина VK в сеть. Создаётся ОДИН на токен (вместе с ним создаётся троттлер; два клиента означали бы двойной бюджет запросов). call/call_value троттлятся и ретраятся; upload_multipart, get_stream и longpoll_check — нет. access_token подставляется только в post_once и едет в ТЕЛЕ запроса. Debug печатает <redacted>. Ячейка ConnHealth разделяется с VkState через health_handle().
