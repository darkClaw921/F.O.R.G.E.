// tmux-web — лёгкий markdown-renderer (общий модуль).
//
// Вынесен из echo/chat.js, чтобы рендером можно было пользоваться и в других
// вью (например, daily-summary). Чистый DOM-renderer без сторонних библиотек:
// code fences (```...```), inline `code`, **bold**, *italic*, заголовки
// (#, ##, ###), bullet-списки, ссылки [text](url).
//
// XSS-безопасность: строим DOM через document.createElement и textContent,
// пользовательские данные НЕ уходят в innerHTML. Это намеренно — vanilla
// bundle без CDN-зависимостей.
//
// Pure leaf-module: импортирует только браузерный DOM.

/**
 * Рендерит markdown-текст в DOM-узлы внутри `container` (добавляет дочерние
 * элементы, не очищает контейнер — это ответственность вызывающего).
 *
 * @param {HTMLElement} container — куда добавлять отрендеренные узлы
 * @param {string} text — исходный markdown-текст
 */
export function renderMarkdownInto(container, text) {
    const lines = String(text).split(/\r?\n/);
    let i = 0;
    while (i < lines.length) {
        const line = lines[i];
        // Code fence
        const fence = line.match(/^```(\w*)\s*$/);
        if (fence) {
            const lang = fence[1] || '';
            const collected = [];
            i++;
            while (i < lines.length && !/^```\s*$/.test(lines[i])) {
                collected.push(lines[i]);
                i++;
            }
            // skip closing ```
            if (i < lines.length) i++;
            const pre = document.createElement('pre');
            pre.className = 'echo-md-pre';
            if (lang) pre.dataset.lang = lang;
            const code = document.createElement('code');
            code.textContent = collected.join('\n');
            pre.appendChild(code);
            container.appendChild(pre);
            continue;
        }
        // Heading
        const heading = line.match(/^(#{1,3})\s+(.*)$/);
        if (heading) {
            const lvl = heading[1].length;
            const h = document.createElement(`h${lvl + 2}`); // ## → h4
            h.className = 'echo-md-h';
            appendInline(h, heading[2]);
            container.appendChild(h);
            i++;
            continue;
        }
        // Bullet list (collect consecutive)
        if (/^\s*[-*]\s+/.test(line)) {
            const ul = document.createElement('ul');
            ul.className = 'echo-md-ul';
            while (i < lines.length && /^\s*[-*]\s+/.test(lines[i])) {
                const li = document.createElement('li');
                appendInline(li, lines[i].replace(/^\s*[-*]\s+/, ''));
                ul.appendChild(li);
                i++;
            }
            container.appendChild(ul);
            continue;
        }
        // Ordered list (collect consecutive `1. `, `2. `, ...)
        if (/^\s*\d+\.\s+/.test(line)) {
            const ol = document.createElement('ol');
            ol.className = 'echo-md-ol';
            while (i < lines.length && /^\s*\d+\.\s+/.test(lines[i])) {
                const li = document.createElement('li');
                appendInline(li, lines[i].replace(/^\s*\d+\.\s+/, ''));
                ol.appendChild(li);
                i++;
            }
            container.appendChild(ol);
            continue;
        }
        // Blank line
        if (line.trim() === '') {
            i++;
            continue;
        }
        // Paragraph (collect until blank or special)
        const para = [line];
        i++;
        while (i < lines.length && lines[i].trim() !== ''
               && !/^(#{1,3})\s/.test(lines[i])
               && !/^```/.test(lines[i])
               && !/^\s*[-*]\s/.test(lines[i])
               && !/^\s*\d+\.\s/.test(lines[i])) {
            para.push(lines[i]);
            i++;
        }
        const p = document.createElement('p');
        p.className = 'echo-md-p';
        appendInline(p, para.join('\n'));
        container.appendChild(p);
    }
}

/**
 * Проверяет, что URL ссылки имеет безопасную схему. Разрешены только
 * http/https/mailto и относительные ссылки (без схемы). Блокирует
 * `javascript:`, `data:`, `vbscript:` и прочие XSS-векторы в href.
 *
 * @param {string} url
 * @returns {boolean}
 */
function isSafeUrl(url) {
    const s = String(url).trim();
    // Относительные/якорные ссылки без схемы безопасны.
    // Схема = последовательность [a-z0-9+.-] перед первым ':' до '/', '?', '#'.
    const schemeMatch = s.match(/^([a-zA-Z][a-zA-Z0-9+.-]*):/);
    if (!schemeMatch) {
        // Нет явной схемы — относительная ссылка. Но отвергаем хитрые случаи
        // вроде " javascript:..." (ведущие пробелы уже сняты trim) и
        // protocol-relative '//host' оставляем как безопасные (наследует http/https).
        return true;
    }
    const scheme = schemeMatch[1].toLowerCase();
    return scheme === 'http' || scheme === 'https' || scheme === 'mailto';
}

function appendInline(parent, text) {
    // Inline: `code`, **bold**, *italic*, [text](url)
    // Простой regex-token парсер. Не покрывает все edge-cases markdown,
    // но достаточен для типичных ответов Claude.
    const re = /(\[[^\]]+\]\([^)]+\))|(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*]+\*)/g;
    let last = 0;
    let m;
    while ((m = re.exec(text)) !== null) {
        if (m.index > last) {
            parent.appendChild(document.createTextNode(text.slice(last, m.index)));
        }
        const tok = m[0];
        if (tok.startsWith('`')) {
            const c = document.createElement('code');
            c.className = 'echo-md-code';
            c.textContent = tok.slice(1, -1);
            parent.appendChild(c);
        } else if (tok.startsWith('**')) {
            const b = document.createElement('strong');
            b.textContent = tok.slice(2, -2);
            parent.appendChild(b);
        } else if (tok.startsWith('*')) {
            const i = document.createElement('em');
            i.textContent = tok.slice(1, -1);
            parent.appendChild(i);
        } else if (tok.startsWith('[')) {
            const linkMatch = tok.match(/^\[([^\]]+)\]\(([^)]+)\)$/);
            if (linkMatch && isSafeUrl(linkMatch[2])) {
                const a = document.createElement('a');
                a.href = linkMatch[2];
                a.textContent = linkMatch[1];
                a.target = '_blank';
                a.rel = 'noopener noreferrer';
                parent.appendChild(a);
            } else {
                // Невалидная схема (javascript:/data: и пр.) или битый токен —
                // не создаём <a>, рендерим как обычный текст. Закрывает XSS
                // через markdown-ссылки в ответах Claude.
                parent.appendChild(document.createTextNode(tok));
            }
        }
        last = re.lastIndex;
    }
    if (last < text.length) {
        parent.appendChild(document.createTextNode(text.slice(last)));
    }
}
