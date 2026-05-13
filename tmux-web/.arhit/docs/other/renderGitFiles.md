# renderGitFiles

Функция в static/app.js (Phase 4, ~строка 1376). Перерисовывает оба списка файлов (#git-staged-list и #git-unstaged-list) на основе state.gitStatus.files.

Алгоритм:
1. innerHTML = '' для обоих списков.
2. Если !state.gitStatus || !state.gitStatus.repo — оставляем пустыми (toolbar уже показал плейсхолдер).
3. Для каждого f в state.gitStatus.files (с защитой Array.isArray):
   - Skip если !f || typeof f.path !== 'string'.
   - if (f.staged) — append buildGitFileRow(f, true) в $gitStaged.
   - else — append buildGitFileRow(f, false) в $gitUnstaged.

buildGitFileRow создаёт <label class='git-file-row'> с:
- <input type='checkbox'> с dataset.path=f.path, .checked=stagedSection. Conflict-файлы (kind='conflict') — disabled с title-подсказкой 'Сначала разрешите конфликт'. onChange → toggleStage([f.path], stagedSection ? 'unstage' : 'stage').
- Два <span class='git-badge {kind-class}'> для X и Y порций кода (с GIT_KIND_CLASSES маппингом). Пробелы заменяются на '·' чтобы badge не схлопывался.
- <span class='path'> с textContent (защита от XSS для путей с <script>-подобными именами). Renamed-файлы: 'orig_path → new_path'.

Зависит от: state.gitStatus, GIT_KIND_CLASSES, toggleStage, DOM-элементы $gitStaged/$gitUnstaged.

См. также: renderGitToolbar, toggleStage, buildGitFileRow.
