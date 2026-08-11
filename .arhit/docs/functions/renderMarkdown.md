# renderMarkdown

js/messenger/markdown.js — рендерер AST в DocumentFragment. Строит узлы ИСКЛЮЧИТЕЛЬНО через doc.createElement / doc.createTextNode / appendChild / setAttribute: ни innerHTML, ни insertAdjacentHTML, ни outerHTML, ни шаблонных строк с разметкой (инвариант файла, его сторожит секция 4 теста).

doc передаётся ПАРАМЕТРОМ, а не берётся из глобального document — именно это делает рендерер проверяемым на фейковом документе в Node. Каждый вызов строит НОВЫЕ узлы: один и тот же AST можно отрисовать дважды (попап разбора и тред) без разделяемых нод. Не-массив на входе даёт пустой фрагмент.

Соответствие блоков тегам: paragraph -> p.md-p; heading -> h1..h6.md-h (уровень зажимается в 1..6); hr -> hr.md-hr; code -> pre.md-pre > code.md-code-block с data-lang (подсветки НЕТ — решение ТЗ, язык сохранён атрибутом, чтобы подсветку можно было добавить позже без правки парсера; мусорные значения языка отбрасываются safeLang); blockquote -> blockquote.md-quote с рекурсивным рендером вложенных блоков; list -> ul.md-ul / ol.md-ol с атрибутом start (ставится, только если стартовый номер не 1: «см. пункт 5» в ответе модели не должно врать), пункты — li.md-li с вложенными списками внутри; table -> div.md-table-wrap > table.md-table > thead/tbody/tr/th/td.

ТАБЛИЦА ВСЕГДА ОБЁРНУТА в контейнер .md-table-wrap со своим горизонтальным скроллом (css/vk.css): широкая таблица обязана скроллиться внутри себя, а не растягивать попап разбора вместе с полем вопроса. Выравнивание ячеек применяется через style.textAlign, а не устаревший атрибут align.

Инлайн: text -> текстовый узел, code -> code.md-code, strong -> strong, em -> em, strike -> s, link -> renderLink (см. safeHref).

Рядом: renderMarkdownText(text, doc) — parse + render одним вызовом, то, что нужно вызывающему коду панели: out.appendChild(renderMarkdownText(answer, document)).
