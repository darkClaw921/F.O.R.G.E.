# buildVoiceForm

tmux-web/static/js/settings/voice-tab.js — секция «Голосовые сообщения» в настройках. ОДИН модуль на три вкладки (VK, Telegram, Битрикс24), различия в конфигах VK_VOICE_CONFIG / TELEGRAM_VOICE_CONFIG / B24_VOICE_CONFIG; прецедент параметризации в этом каталоге — hotkeys-tab.js. Монтируется из settings/modal.js третьим блоком панели, после видимости и клавиш.

Сигнатура: buildVoiceForm(settings, onSaved, config, api, doc) -> <fieldset>. api — ССЫЛКИ на getVoiceSettings/setVoiceSettings соответствующей панели (адреса живут в её api.js, где их сверяет контрактный тест с таблицей роутов плагина); без api селект движка не рисуется, чекбокс остаётся.

Что внутри:
1. Чекбокс «Автоматически разбирать новые аудиосообщения» -> своё поле user_settings (vk_/telegram_/bitrix_voice_autotranscribe). Дефолт ВЫКЛЮЧЕН и читается строго '=== true'. Сохраняется СРАЗУ по change (настройка одна, кнопка Save осмысленна там, где черновик собирается из десятков галочек). Отказ PATCH откатывает галочку и печатает причину рядом — молча вернувшийся чекбокс читался бы как поломка UI.
2. Селект движка (Локально / Облако) — ОБЩИЙ для всех мессенджеров, читается и пишется роутом .../voice/settings той панели, чья вкладка открыта; под ним подпись, что настройка общая. До ответа бэкенда селект выключен: показать 'Локально' там, где на самом деле облако, хуже, чем показать загрузку. warning бэкенда (voice_no_key, voice_no_model) печатается человеку — иначе причину можно узнать только открыв voice.toml. Отказ PUT возвращает прежнее значение.

Стили — .voice-engine-select / .voice-engine-shared / .voice-engine-warning / .voice-save-ok в css/settings-modal.css, только токенами палитры. Тест: tmux-web/tests/frontend/settings_voice_tab.test.mjs (42 проверки).
