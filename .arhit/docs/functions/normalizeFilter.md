# normalizeFilter

tmux-web/static/js/vk/visibility.js::normalizeFilter(settings) -> {enabled, kinds, peers, active}.

Приводит три поля настроек (vk_filter_enabled, vk_visible_kinds,
vk_visible_peers) к рабочему фильтру: чистит мусор (пробелы, дубли, не-массив),
приводит id к строкам.

ГЛАВНЫЙ ИНВАРИАНТ — «ВКЛЮЧЁН НЕ ЗНАЧИТ ДЕЙСТВУЕТ»: active = enabled &&
(kinds.length || peers.length). Включённый тумблер с пустым выбором ведёт себя
как выключенный, иначе пользователь получил бы пустую панель и прочитал бы её
как поломку. Выбор при этом СОХРАНЯЕТСЯ при выключении тумблера — снятая
галочка не должна стирать список.

Фильтр применяется в ЧЕТЫРЁХ точках, и пропуск любой = утечка скрытой беседы:
колонка (dialogs.js::visibleSource), счётчики серверных фильтров (renderDirs),
бейдж (main.js::recomputeUnread), тосты и накопление непрочитанного
(maybeToast/noteUnknownUnread). Регресс-тесты — vk_visibility.test.mjs и
vk_dialogs_filter.test.mjs.

ПОБОЧНОЕ СЛЕДСТВИЕ: при действующем фильтре серверный unread_total игнорируется
целиком — он посчитан по всему аккаунту, расщепить его нечем. Это осознанный
размен, а не забытая ветка.
