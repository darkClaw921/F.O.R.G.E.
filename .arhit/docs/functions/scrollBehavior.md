# scrollBehavior

scrollBehavior(reduceMotion) -> 'auto' | 'smooth' (tmux-web/static/js/telegram/follow.js) — режим прокрутки для CSSOM `Element.scrollTo({top, behavior})`.

КОНТРАКТ: reduceMotion === true ⇒ 'auto' (мгновенно), иначе ⇒ 'smooth' (плавно). Сравнение строгое: любое не-true (undefined, 0, '') трактуется как «анимация разрешена».

ЗАЧЕМ ОТДЕЛЬНАЯ ФУНКЦИЯ ИЗ ОДНОЙ СТРОКИ. Правило проекта «уважать prefers-reduced-motion» проверяемо только если оно вынесено из DOM-кода: сам медиазапрос в Node не воспроизвести, а вот отображение «настройка → режим» покрывается ассертом. Плюс это единственная точка, где решение о плавности принимается, — режим нельзя случайно захардкодить в двух местах.

ЧТЕНИЕ САМОЙ НАСТРОЙКИ живёт в chat.js::prefersReducedMotion: `window.matchMedia('(prefers-reduced-motion: reduce)').matches` под двойным typeof-гардом (в node-стендах `window.matchMedia` не определён) и в try/catch. Медиазапрос читается В МОМЕНТ КАЖДОГО ДОСКРОЛЛА, а не один раз на импорте, — пользователь может переключить системную настройку не перезагружая вкладку (тест telegram_scroll_ui.test.mjs секция 6 проверяет переключение МЕЖДУ вызовами).

ПОТРЕБИТЕЛЬ: chat.js::scrollBlockIntoView. Прежний путь scrollToBottom (прилипание к низу) остался мгновенным присваиванием scrollTop и prefers-reduced-motion не касается — там анимации никогда и не было.
