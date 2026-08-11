# plugins/telegram/src/routes/analysis.rs

Роуты «Анализа чата через Claude» плагина Telegram: 7 маршрутов, 8 путей.

ПУТИ:
- GET/PUT /api/telegram/dialogs/:id/prompt — заметка пользователя к диалогу (пустая строка стирает).
- GET /api/telegram/dialogs/:id/analysis — последний сохранённый разбор + тред Q&A.
- POST /api/telegram/dialogs/:id/analysis — запуск разбора (kind: analyze | refresh | ask).
- DELETE /api/telegram/dialogs/:id/analysis — стереть разбор (тред и заметка целы).
- DELETE /api/telegram/dialogs/:id/analysis/thread — стереть тред (разбор и заметка целы).
- POST /api/telegram/dialogs/:id/analysis/cancel — остановить идущий run.

ДВЕ ЛЕСТНИЦЫ НА ОДНОМ ПУТИ. У /analysis разные методы стоят на разных лестницах, и это не непоследовательность. GET/DELETE работают с локальным файлом (как оба /prompt) и соединения НЕ требуют — иначе разбор прятался бы ровно тогда, когда сеть отвалилась, то есть когда он полезнее всего. POST /analysis и /analysis/cancel требуют require_ready: разбор начинается с выгрузки истории и без Telegram не существует.

ЛЕСТНИЦА ЛОКАЛЬНЫХ: авторизация (409) -> резолв peer (404/503) -> разбор тела (400). Вынесена в хелпер local_ladder, который сразу отдаёт канонический ключ хранилища.

ПОРЯДОК ОТКАЗОВ POST /analysis ЗАФИКСИРОВАН: require_ready -> resolve_peer -> тело и kind -> уже идущий run по этому диалогу -> наличие Claude CLI -> свободный слот -> выгрузка истории -> старт. Три последних проверки стоят ДО выгрузки не для красоты: выгрузка это до пяти RPC, и отказ «уже идёт два анализа» после полутора секунд выглядит как поломка.

КАНОНИЧЕСКИЙ КЛЮЧ (storage_key). У Telegram resolve_peer async и возвращает PeerRef; ключом файла заметок служит dto::dialog_id_string(peer.id), а не сырой сегмент URL. Вынесен в отдельную функцию намеренно: это единственное место конвертации, и разъехавшийся ключ означал бы «разбор пропал после перезахода», причём молча.

ИНВАРИАНТЫ AUTH-LADDER: тело всегда Option<Json<T>>, никогда Json<T>; никакого Query<T>; у треда ОТДЕЛЬНЫЙ путь, а не ?thread=1. Причина общая: типовые экстракторы отрабатывают ДО тела хендлера и ответили бы 400 раньше проверки авторизации, превратив роут в оракул валидности идентификаторов.

kind=refresh И «НОВЫХ НЕТ»: штатный ответ 200 {run_id: null, no_new_messages: true}, модель не запускается. Проверка идёт по ТРАНСКРИПТУ (transcript.messages_used == 0), а не по числу выгруженных сообщений: записи без текста и медиа в транскрипт не попадают, и запуск модели на пустом transcript ЗАМЕНИЛ БЫ хороший разбор ответом «ничего нового не произошло».

ОТМЕНА: финальный кадр analysis_done{ok:false, reason:cancelled} шлёт именно роут — пампер после abort'а уже мёртв. Неизвестный run НЕ ошибка (200 cancelled:false): гонка «нажал стоп ровно когда модель дописала» штатна.

map_analysis_error — единая таблица перевода AnalysisError ядра в ApiError: Busy->409 invalid_state, CliUnavailable->409 claude_unavailable (с путём), NoPreviousAnalysis->400 invalid_input, Storage->500 storage_failed. Коды совпадают с VK и Bitrix один в один: их разбирает ОДИН общий фронтовый модуль js/messenger/analysis-core.js.

build_task — тело запроса в forge_chat_analysis::prompt::Task с проверкой закрытого списка kind, обязательности вопроса для ask и наличия сохранённого разбора для refresh.
