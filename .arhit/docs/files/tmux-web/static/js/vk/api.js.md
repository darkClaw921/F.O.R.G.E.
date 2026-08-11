# tmux-web/static/js/vk/api.js

REST-клиент панели VK: тонкие обёртки поверх apiFetch через разборщик конверта js/messenger/http-envelope.js. Шапка файла — ЗАМОРОЖЕННАЯ таблица путей, обязанная совпадать дословно с шапкой plugins/vk/src/routes/mod.rs (расхождение даёт молчаливый 404, а не ошибку сборки; сверяет vk_contract.test.mjs).

Группы: статус и логин (getStatus, loginToken, verifyLogin, logout, submitCaptcha, captchaImageUrl), данные (getFilters, getDialogs, getMessages, getMessage, sendMessage, markRead, sendTyping, pressButton, mediaUrl, fetchMedia), анализ чата через Claude.

АНАЛИЗ ЧАТА (АЧ-Ф1/Ф3 + доработка АЧ-И1):
* getChatPrompt / putChatPrompt — пер-чатовая заметка пользователя; max_chars приезжает С СЕРВЕРА.
* getSavedAnalysis(dialogId) — GET /api/vk/dialogs/:id/analysis, последний СОХРАНЁННЫЙ разбор {text, last_message_id, messages_used, updated_at, max_chars}. Отвечает 200 и при мёртвой сети VK (разбор лежит в локальном файле рядом с заметкой). «Разбора нет» — это пустой text, а НЕ 404.
* clearSavedAnalysis(dialogId) — DELETE того же пути; заметку не трогает, {cleared:false} значит «стирать было нечего».
* startAnalysis(dialogId, {kind, question?, depth?, history?}) — POST. Три kind: analyze, ask, refresh. У refresh два особых исхода: нет сохранённого разбора → 400 invalid_input; нет новых сообщений → 200 {run_id: null, no_new_messages: true} (модель НЕ запускалась, кадров по /ws/vk не будет — ради этого случая run_id стал nullable).
* cancelAnalysis(dialogId, runId?) — POST …/analysis/cancel; без runId сервер гасит run текущей беседы.

Все вызовы анализа идут через callC, а не голый call: код 14 (captcha) у VK прилетает на любой метод, включая выгрузку истории внутри POST /analysis, и ошибка обязана донести err.captcha до js/vk/captcha.js. Место в бюджете 3 rps берёт ВЫЗЫВАЮЩИЙ (analysis.js делает vkLimiter.acquire перед каждым вызовом) — планировщик сюда не импортируется, api.js остаётся чистым транспортом.

Инвариант «браузер на серверы VK не ходит»: в модуле нет ни одной ссылки на api.vk.com/vk.com/vk.ru — и captcha, и медиа идут через прокси бэкенда (проверяется vk_contract.test.mjs).
