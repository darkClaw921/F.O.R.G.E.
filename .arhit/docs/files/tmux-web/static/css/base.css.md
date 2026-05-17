# tmux-web/static/css/base.css

Базовые стили tmux-web: CSS-переменные тёмной темы (--bg, --fg, --accent, --border, и т.д.), правила :root, reset (html/body, * box-sizing), глобальные fallback-шрифты. Исходник: первые 128 строк бывшего монолитного style.css. Должен импортироваться ПЕРВЫМ через @import — все остальные css-файлы полагаются на var(--*) и базовый reset.
