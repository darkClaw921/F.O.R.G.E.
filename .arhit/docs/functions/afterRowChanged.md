# afterRowChanged

afterRowChanged(row, ctx) (tmux-web/static/js/telegram/chat.js, внутренняя) — ЕДИНАЯ точка «что делать с лентой после того, как строка появилась или пересобралась».

ПАРАМЕТРЫ: row — актуальный DOM-узел строки (после replaceWith — новый!); ctx = {atBottom, following} — замеры, сделанные ВЫЗЫВАЮЩИМ.

ПОВЕДЕНИЕ по follow.js::scrollDecision(ctx):
 * 'follow' ⇒ observeFollowRow(row) + scrollBlockIntoView(row). Наблюдение переезжает на актуальный узел, потому что после `row.replaceWith(fresh)` старый уже вне документа, а рост (медиа, клавиатура) ещё впереди;
 * 'stick' ⇒ scrollToBottom() — прежнее поведение: доскролл только тому, кто и так был у нижней кромки;
 * 'none' ⇒ ничего.

ПОЧЕМУ ЗАМЕРЫ ПРИХОДЯТ ПАРАМЕТРОМ, А НЕ СЧИТАЮТСЯ ЗДЕСЬ. isAtBottom обязан быть измерен ДО мутации DOM: после appendChild/replaceWith высота ленты уже новая, и функция отвечает про ленту, которой пользователь ещё не видел. Замерять внутри afterRowChanged было бы поздно по построению. Это и есть корень исходного дефекта — в onEdited замера не было вовсе.

КТО ЗОВЁТ (три места):
 * onNewMessage — ctx = {atBottom: isAtBottom($tgLog) до вставки, following: isFollowing(dialogId, Date.now())} (сообщение НОВОЕ, сверять message_id не с чем);
 * onEdited, ветка полной пересборки — с узлом `fresh`;
 * onEdited, ветка точечного патча текста — с исходным узлом `row`.
В обеих ветках onEdited ctx.following = followsMessage(dialogId, msg.id, Date.now()) — здесь адресность строгая.

СВЯЗАННЫЕ: follow.js::scrollDecision, scrollBlockIntoView, scrollToBottom, observeFollowRow, format.js::isAtBottom.
