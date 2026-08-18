# collectUrlsFromSegments

messenger/meet-core.js::collectUrlsFromSegments(segments, depth, acc) — адреса из дерева BB-сегментов Битрикс24, в порядке обхода.

ПОЧЕМУ НЕ segmentsText(). bitrix/format.js::segmentsText возвращает 'inner || value', поэтому у [URL=https://telemost.yandex.ru/j/1]созвон[/URL] отдаст «созвон», а адрес потеряется совсем (проверено тестом: плоский текст такого сегмента не содержит подстроки telemost). Здесь наоборот: у сегмента kind='url' берётся сначала value, а затем содержимое.

ДВЕ ЖИВЫЕ ФОРМЫ ПОРТАЛА, и они прямо противоположны:
1) [URL=адрес]подпись[/URL] — адрес в seg.value, подпись в children;
2) [URL]https://…[/URL] — атрибута НЕТ, seg.value === null, адрес лежит ТЕКСТОМ внутри. Так портал сам оборачивает голую ссылку при URL_PREVIEW='Y' (закреплено в plugins/bitrix/src/bbcode.rs::a_bare_url_wrapped_by_the_portal_keeps_its_address).
Обе встречаются в одном чате, поэтому берутся обе.

Текстовые сегменты (kind='text') тоже просматриваются через collectUrlsFromText: с выключенным превью портал не оборачивает ссылку ничем, и она приезжает обычным текстом. Обход рекурсивный — ссылка внутри [QUOTE] и [B] находится.

Ограничитель глубины 8 — как у renderSegments, по той же причине: дерево приходит от портала. Потолок кандидатов 32.

Связанные: bitrix/format.js::segmentsText (чего она не умеет), bitrix/format.js::renderSegments (откуда взят ограничитель), messageUrlCandidates.
