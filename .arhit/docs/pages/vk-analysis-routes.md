Роуты запуска анализа чата VK (plugins/vk/src/routes/analysis.rs, АЧ-Ф3).

ПОВЕРХНОСТЬ (дополняет GET/PUT /api/vk/dialogs/:id/prompt из АЧ-Ф1):
  POST /api/vk/dialogs/:id/analysis         — запустить анализ или вопрос по переписке
  POST /api/vk/dialogs/:id/analysis/cancel  — остановить идущий анализ

ДВЕ ЛЕСТНИЦЫ В ОДНОМ МОДУЛЕ. /prompt: авторизация -> резолв peer -> тело (соединения НЕ требует, локальный файл). /analysis и /analysis/cancel: require_ready (авторизация 409 -> соединение 503) -> resolve_peer (404) -> тело (400). Ступень соединения у анализа обязательна: он начинается с выгрузки истории через messages.getHistory, то есть без VK не существует вовсе.

ПОРЯДОК ОТКАЗОВ У POST /analysis ЗАФИКСИРОВАН: require_ready -> resolve_peer -> тело и kind -> уже идущий run по этой беседе (200 attached) -> наличие Claude CLI (409 claude_unavailable) -> свободный слот (409 invalid_state) -> выгрузка истории -> старт. Три последних проверки стоят ДО выгрузки не для красоты: выгрузка это до ~1.7 с запросов при бюджете 3 rps, и отказ 'уже идёт два анализа' после полутора секунд ожидания выглядит как поломка, а повторный клик не имеет права стоить второго обхода истории.

DTO. AnalysisReq { kind: 'analyze'|'ask', question: Option<String>, depth: Option<i64>, history: Option<Vec<QaTurnDto{q,a}>> }; AnalysisResp { run_id, attached, messages_used, truncated }; CancelReq { run_id: Option<String> }; CancelResp { cancelled }.

ПРАВИЛА. kind вне закрытого списка -> 400; question обязателен при kind=ask; depth клампится context::clamp_depth (дефолт 200). Тело — Option<Json<T>>, НЕ Json<T>: экстрактор Json отверг бы битый JSON раньше авторизации и покрасил бы routes::tests::hostile_dialog_ids_hit_auth_before_any_parsing. Никакого Query<T>.

ХОД post_analysis. Собрать транскрипт (context::fetch + context::render с бюджетом cfg.analysis_max_chars) -> взять имя беседы из прогретого кэша peers и пер-чатовую заметку -> prompt::build -> AnalysisHub::start (спавнит пампер) -> ответ уходит СРАЗУ. Текст модели приезжает кадрами analysis_delta, финал — analysis_done. Держать HTTP открытым на всю генерацию нельзя: она длится минуты, любой прокси оборвёт.

ОТМЕНА. Тело необязательно: без run_id гасится тот run, что идёт по этой беседе (кнопка 'стоп' обязана работать после перезагрузки вкладки, потерявшей run_id). Неизвестный run — НЕ ошибка: 200 {cancelled:false}; гонка 'нажал стоп ровно когда модель дописала' штатна. Финальный кадр analysis_done{ok:false,reason:cancelled} шлёт именно этот роут — пампер после abort'а мёртв.

Строки роутов продублированы в таблице-контракте шапки plugins/vk/src/routes/mod.rs; фронтовая половина таблицы (js/vk/api.js) заполняется в АЧ-Ф4.