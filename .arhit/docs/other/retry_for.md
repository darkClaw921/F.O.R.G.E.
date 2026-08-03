# retry_for

plugins/vk/src/api/error.rs. retry_for(&VkError) -> Retry — политика повторов. Транспорт, HTTP 5xx и код VK 10 -> AfterNetworkBackoff (500 мс + джиттер 0-250 мс). Код 6 -> AfterOneSecond: это промах НАШЕГО троттлера, молча починить честнее, чем показать пользователю 429 за собственную ошибку. Коды 9 и 29 -> Never: автоповтор продлевает бан, прямой аналог FLOOD_WAIT у Telegram. HTTP 4xx и Decode -> Never. Повтор всегда не более одного: второй отказ это уже состояние, а не сетевая рябь.
