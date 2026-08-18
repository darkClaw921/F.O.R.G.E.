# regenerateSummary

Клиент POST /api/meet/meetings/:id/summary в static/js/meet/api.js — пересчёт саммари поверх готовой расшифровки. Ровно то же, что retryMeeting(id, 'summarizing'): у операции два имени, потому что пересчёт саммари — самостоятельное действие в карточке, а не частный случай повтора этапа. Кнопка ПЕРЕСЧИТАТЬ САММАРИ в meet-popup.js ходит через retryMeeting; этот вызов оставлен явным именем для внешних клиентов и объявлен в таблице MEET_ROUTES, которая дословно сверяется с шапкой plugins/meet/src/routes/mod.rs тестом meet_contract.test.mjs.
