# tmux-web/static/js/main.js

Phase 1 entry-point. Порядок: 1) import './core/auth.js' (side-effect: подмена window.fetch + token из location.hash) — ПЕРВЫМ. 2) import './public-api.js' (window.ForgeApp). 3) import { bootstrap } + вызов bootstrap() с проверкой document.readyState. Подключается из index.html как <script type=module src=/js/main.js>.
