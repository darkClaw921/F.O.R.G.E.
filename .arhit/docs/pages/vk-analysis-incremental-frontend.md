Доработка «сохраняемый и инкрементальный анализ чата VK» — фронтовая половина (эпик forge-vkinc-e2-pd5k, 9 задач).

ПОЧЕМУ. Жалоба из живого использования: задал вопрос по чату — разбор пропал, надо анализировать заново. Плюс не было способа догнать переписку и не было кнопок очистки.

ЧТО СДЕЛАНО.
1. Разметка (tmux-web/static/index.html, блок .vk-analysis-controls): кнопки #vk-analysis-refresh («REFRESH ↻», disabled в разметке), #vk-analysis-clear («CLEAR ✕»), #vk-analysis-clear-thread («CLEAR Q&A») и строка #vk-analysis-updated. Все четыре id заведены в tmux-web/static/js/core/dom.js и в фейковом DOM tests/frontend/vk_dom_stub.mjs.
2. tmux-web/static/js/vk/api.js: getSavedAnalysis (GET) и clearSavedAnalysis (DELETE) + kind:'refresh' и nullable run_id в контракте startAnalysis; новые строки в замороженной таблице путей.
3. tmux-web/static/js/vk/analysis.js: #vk-analysis-out закреплён за разбором, ответ на вопрос уходит в свою строку #vk-analysis-thread (deltaTarget/askRow), clearOut() больше не вызывается при kind==='ask'; ленивый GET сохранённого разбора при открытии попапа и при смене беседы; REFRESH с обработкой no_new_messages; две кнопки очистки.
4. tmux-web/static/css/vk.css: .vk-analysis-updated (вторичная строка, :empty не занимает места) и margin-top:0 у .vk-analysis-btn — иначе .vk-login-btn-ghost ронял бы вторичные кнопки ниже основных в ряду из пяти.

ТРИ РЕШЕНИЯ, КОТОРЫЕ ЛЕГКО СЛОМАТЬ ОБРАТНО.
* Приёмник дельт выбирается по наличию открытой строки треда, а НЕ по local.kind: kind приезжает кадром analysis_started и может опоздать к первым дельтам.
* REFRESH стирает прошлый разбор не на старте, а перед ПЕРВОЙ дельтой (local.replaceOut): упавший на запуске claude иначе оставлял бы пустой экран при живом разборе на сервере.
* startRun НЕ проверяет analysisBusy. Проверка была — и живая приёмка на ней споткнулась: ANALYZE, нажатый в первые доли секунды после открытия попапа (пока идёт GET сохранённого разбора), молча не срабатывал при доступной кнопке.

ПРИЁМКА. cargo build --workspace и cargo test --workspace --test-threads=1 зелёные; cargo clippy --workspace --all-targets без новых предупреждений; все tmux-web/tests/frontend/*.test.mjs зелёные (vk_analysis 125 ассертов, vk_contract 35/35); npx playwright test tests/ui/vk-ui.spec.ts 14/14.

ЖИВАЯ ПРИЁМКА (изолированный инстанс: свой HOME, свободный порт, мок api.vk.com, мок claude CLI) — все шесть шагов прошли: разбор → вопрос (разбор остался на месте, ответ в треде) → новое сообщение в чате → REFRESH (в промпт модели уехал ровно один новый message и блок previous_analysis, метка пересчиталась) → перезагрузка страницы (разбор восстановился) → CLEAR анализа (пусто, REFRESH заблокирован) → CLEAR Q&A (тред пуст, ноль запросов в сеть). REFRESH без новых сообщений дал статус «новых сообщений нет» без запуска модели.