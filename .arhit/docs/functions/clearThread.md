# clearThread

ДВЕ функции с этим именем, и путать их нельзя.

1) js/vk/api.js::clearThread(dialogId) — транспорт: DELETE /api/vk/dialogs/:id/analysis/thread через общий разборщик конверта (js/messenger/http-envelope.js). Стирает ТОЛЬКО тред вопросов; ни разбор, ни заметку беседы не трогает. Ответ {cleared: boolean}; cleared:false — не ошибка, а «стирать было нечего» (две вкладки нажали подряд). Отдельный путь, а не ?thread=1 у общего DELETE: query-параметры в плагине разбираются вручную (RawQuery), потому что Query<T> ломает auth-лестницу маршрута.

2) js/vk/analysis.js::clearThread() — обработчик кнопки CLEAR Q&A: async, шлёт ровно один запрос через vkApi.clearThread под лимитером (PRIORITY_USER), и ТОЛЬКО по успеху чистит local.thread, local.askRow и DOM #vk-analysis-thread. При ошибке тред остаётся на экране (на сервере он цел), текст ошибки выбирается по err.code. На время запроса кнопка гаснет (local.threadBusy), во время идущего run'а запрос не уходит вовсе. Блок #vk-analysis-out не трогается ни в одной ветке — это вторая кнопка.
