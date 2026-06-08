Фича «Заставка — таверна дворфов» (добавлена 2026-06-04).

Назначение: рядом с кнопкой настроек (⚙ #project-settings) добавлена кнопка 🍺 (#screensaver-toggle); по нажатию вместо области сессий открывается полноэкранная ASCII-анимация — дворфы пьют в таверне, поднимают кружки и переговариваются репликами в облачках над головами (дев-юмор F.O.R.G.E.). Заставка кликабельна: клик по дворфу поднимает кружку и выдаёт новую реплику, иногда сосед отвечает.

Архитектурное решение: повторяет паттерн «ежедневной сводки» (#daily-summary) — оверлей position:absolute;inset:0 в #main, показ через display:flex + скрытие #home + showPlaceholder(false), выход через «← Назад»/Esc → fetchSessions(). Чисто фронтенд, бэкенд не требуется.

Затронутые файлы:
- НОВЫЙ static/js/screensaver/screensaver.js — логика/анимация (см. doc 'screensaver').
- НОВЫЙ static/css/screensaver.css — стили сцены, дворфов, облачек, кнопки 🍺; @media mobile + prefers-reduced-motion.
- static/index.html — кнопка #screensaver-toggle в #settings-bar; контейнер #screensaver (с #screensaver-back и #ss-stage) в #main.
- static/js/core/dom.js — DOM-ссылки $screensaverToggle/$screensaver/$screensaverBack/$ssStage.
- static/js/core/bootstrap.js — импорт showScreensaver + клик-handler на #screensaver-toggle.
- static/js/ws/attach.js — hideScreensaver() в ws.onopen (гасит при открытии сессии).
- static/js/tabs/tabs.js — hideScreensaver() в начале switchTab (гасит при смене вкладки).
- static/style.css — @import './css/screensaver.css'.

Embedding: ассеты включаются в бинарь автоматически (rust-embed #[folder=...static/]), правок Rust нет — нужна лишь пересборка cargo build -p devforge (выполнена, exit 0).

Остановка анимации (важно для CPU): hideScreensaver отменяет rAF; self-guard в loop (offsetParent===null); внешние хуки в attach/switchTab; Esc; rAF auto-пауза в скрытой вкладке; reduced-motion замораживает кадры.