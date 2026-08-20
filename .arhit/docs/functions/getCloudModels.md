# getCloudModels

tmux-web/static/js/meet/api.js (Ф16): клиент GET /api/meet/cloud/models — каталог облачных моделей, разложенный бэкендом по стадиям ({configured, stt[], chat[], total, error}). Рядом getCloudBalance — GET /api/meet/cloud/balance (остаток в рублях и лимиты ключа).

Оба пути внесены в MEET_ROUTES (замороженный контракт, сверяется в обе стороны с таблицей в шапке plugins/meet/src/routes/mod.rs тестами meet_contract.test.mjs и the_contract_table_and_the_router_agree_in_both_directions). Всего роутов стало 25.

Браузер НЕ ходит в api.polza.ai напрямую: каталог и баланс требуют ключа, и запрос из страницы означал бы ключ во фронте. Отказ облака приходит полем error при статусе 200 — вкладку настроек нельзя ронять чужим сервисом.
