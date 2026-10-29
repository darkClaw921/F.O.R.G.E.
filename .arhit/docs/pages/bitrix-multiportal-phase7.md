Ф7 мультипортальности Битрикс24 — настройки видимости и скоуп голосовых по порталам.

СУТЬ ФАЗЫ. Пять настроек (белый список видимости: filter_enabled/visible_kinds/visible_peers; скоуп авторазбора: voice_scope_kinds/voice_scope_peers) переехали из корня ~/.forge/user_settings.json в карту bitrix_portal_prefs, ключом которой служит portal_id. Причина одна и она непреодолима: visible_peers хранит DIALOG_ID КОНКРЕТНОГО портала, chat7 у двух порталов — разные чаты.

ЧТО ОСТАЛОСЬ ГЛОБАЛЬНЫМ: bitrix_voice_autotranscribe и bitrix_hotkeys*. Тумблер не ссылается ни на какой dialog_id — «разбирать голосовые в Битриксе» это свойство панели.

ЗАТРОНУТЫЕ ФАЙЛЫ:
  tmux-web/src/user_settings.rs — BitrixPortalPrefs, BitrixPortalPrefsPatch, карта, valid_portal_id, MAX_BITRIX_PORTALS=32, apply_bitrix_portal_prefs (миграция при первой записи), merge_with_existing (запись перестала быть разрушительной), bitrix_prefs_for/bitrix_flat_prefs.
  plugins/voice/src/prefs.rs — read_portal + PrefsKeys.portal_prefs + PORTAL_FIELDS; read() сохранён, VK и Telegram не тронуты.
  plugins/bitrix/src/voice_auto.rs — prefs() передаёт state.portal_id().
  tmux-web/static/js/bitrix/visibility.js — portalPrefs / portalPrefsPatch / PORTAL_PREFS_KEY, normalizeFilter(settings, portalId); модуль остался чистым.
  tmux-web/static/js/bitrix/state.js — currentVisibilityFilter(portalId), кэш по порталам, умолчание api.js::activePortal().
  tmux-web/static/js/bitrix/main.js — maybeToast спрашивает фильтр ПОРТАЛА КАДРА.
  tmux-web/static/js/settings/bitrix-settings-portal.js (новый), bitrix-visibility-tab.js (переключатель портала), voice-scope-sources.js (scopeInitial/scopePayload + явный портал), voice-tab.js (два хука для пер-портальной площадки), modal.js (paint), css/settings-modal.css.

МИГРАЦИЯ БЕЗ ПОТЕРЬ И БЕЗ КЛИЕНТА. Пустая карта означает «читай плоские ключи»; первый PATCH, создающий запись, берёт плоские значения основой. Плоские ключи остаются в файле замороженными — откат на старую сборку продолжает работать. Запись файла доливает неизвестные ключи из существующего JSON, поэтому откат не теряет настроек более новой сборки.

СНЯТОЕ ОГРАНИЧЕНИЕ Ф8 (тосты чужих порталов). Подавление осталось ровно там, где вид диалога неизвестен физически: фильтр чужого портала действует ТОЛЬКО по видам, а диалога нет в его загруженном списке (которого у фонового портала и не бывает). Фоновый портал без настроек тостит свободно; поимённо отмеченный диалог тостит без списка. Разбор — в шапке main.js::maybeToast.

ПРОВЕРКИ: cargo build --workspace OK; cargo test -p forge-bitrix 458+66+... OK; cargo test -p forge-voice 163 OK; cargo test -p devforge --bin devforge 714 OK; все node-тесты tmux-web/tests/frontend зелёные (bitrix_visibility 116/116, bitrix_contract 36/36, bitrix_portals 53/53, settings_voice_scope_sources 22/22, settings_bitrix_visibility_form 21/21, settings_voice_tab 71/71).

ИЗВЕСТНЫЙ ДОЛГ (не из этой фазы): tmux-web/tests/bitrix_smoke.rs не компилируется против API реестра порталов — задача forge-axygd, блокирует Ф9.1.