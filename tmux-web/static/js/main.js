// tmux-web — ES Modules entry-point (Phase 1).
//
// Подключается из index.html как `<script type="module" src="/js/main.js">`.
//
// Порядок инициализации обязателен:
//   1) core/auth.js     — side-effect: подмена window.fetch + bootstrap токена
//                          из location.hash. ДОЛЖНО выполниться ДО любого fetch().
//   2) public-api.js    — выставляет window.ForgeApp ДО bootstrap, на случай
//                          если quick-cmd.js дёрнет его сразу.
//   3) bootstrap()      — main wiring (listeners, initial fetches).
//
// Гарантия порядка: `<script type="module">` имеет implicit defer; модули
// выполняются после parse HTML, поэтому DOM-refs из core/dom.js валидны.

import './core/auth.js';
import './public-api.js';
import { bootstrap } from './core/bootstrap.js';

if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bootstrap);
} else {
    bootstrap();
}
