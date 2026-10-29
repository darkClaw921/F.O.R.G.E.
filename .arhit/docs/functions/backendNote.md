# backendNote

tmux-web/static/js/settings/meet-tab.js: подсказка у переключателя стадии встреч. Первым делом проверяет ОТКАТ ГЕЙТА: если status.config.<stage>_fallback_reason непуст и <stage>_effective отличается от заказанного, отдаёт warn «ЗАКАЗАНА … а считается …: причина» — молчаливая подмена движка хуже честного отказа. Дальше прежние проверки: облако без ключа, молчащее облако, исчерпанный лимит, локальный путь без Metal/whisper. Контекст (refs.ctx) собирается из /api/meet/status: cloud, deps, config.
