# tmux-web/static/js/sessions/claude-memory.js

Frontend-модуль модалки «Память Claude» — кнопка 🧠 (#claude-memory-btn) в правом верхнем углу #tab-bar. Теперь с РЕДАКТИРОВАНИЕМ каждой секции памяти на месте.

openClaudeMemoryModal() — строит модалку (шапка .claude-memory-header с h2 + кнопка ↑ Назад, тело .claude-memory-body, actions с Закрыть), берёт cwd активной сессии, вызывает renderMemory().

renderMemory(body, backBtn, backStack, path) — фетчит GET /api/claude-memory?path=<cwd> (новый структурированный ответ {dir, exists, index, files:[{name,content}]}), очищает body и строит по одной секции на index (MEMORY.md) + каждый файл через buildMemorySection. Переиспользуется как onSaved-колбэк после успешного PUT — полная перерисовка проще и надёжнее точечного патча DOM/карты ссылок.

buildMemorySection({name, content, isIndex}, {path, onSaved}) — секция с заголовком (h4.echo-md-h с id для якорей) + кнопкой ✎. По клику на ✎: view (renderMarkdownInto) заменяется на <textarea> с оригинальным сырым содержимым + кнопки Сохранить/Отмена. Сохранить → PUT /api/claude-memory {path, file: name, content: textarea.value}; при 2xx — onSaved() (полная перерисовка модалки); при ошибке — window.alert с текстом ответа сервера, кнопки разблокируются для повтора. Отмена — возврат к view без запроса.

wireInternalMemoryLinks(body, sectionsById, backStack, onNavigate) — перехватывает клики по относительным *.md-ссылкам (напр. [Title](filename.md) внутри MEMORY.md — ссылка на другой файл ТОЙ ЖЕ папки памяти, не URL сервера): если секция с таким именем найдена — скроллит к ней (запомнив позицию в backStack для кнопки '↑ Назад'); если нет — блокирует переход, класс .claude-memory-link-missing.

Кнопка '↑ Назад' (backBtn) — LIFO возврат к позициям скролла до переходов по внутренним ссылкам.

Подключение: core/bootstrap.js (import + $claudeMemoryBtn click), DOM-ref в core/dom.js, кнопка в index.html внутри #tab-bar, стили — css/tab-bar.css (.claude-memory-btn) и css/modals.css (.claude-memory-modal, .claude-memory-header, .claude-memory-back-btn, .claude-memory-section*, .claude-memory-edit-*, .claude-memory-link-missing).

Backend: src/claude_memory.rs::load_project_memory + save_project_memory_file, хендлеры GET/PUT /api/claude-memory в main.rs.
