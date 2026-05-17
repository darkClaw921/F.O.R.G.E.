# tmux-web/static/css/mobile.css

Mobile layout (Phase A): @media (max-width: 768px) — off-canvas сайдбар (transform translateX), увеличенные touch-targets (min-height 44px), горизонтальный kanban (flex-direction row + overflow-x auto), full-screen модалки (inset 0). @media (max-width: 480px) — дополнительное сжатие padding/font-size для узких экранов. @media (max-width: 768px) and (prefers-reduced-motion: reduce) — отключение transitions. 284 строки (2914-3197). Импортируется ПОСЛЕДНИМ в style.css — критично для каскада, чтобы перекрывать desktop-правила.
