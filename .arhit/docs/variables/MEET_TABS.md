# MEET_TABS

Класс корня попапа. По нему его находит `panelVisible()` в keynav всех трёх панелей — не по id: узел создаётся кодом, в `index.html` его нет, и id пришлось бы заводить тремя константами ради одного `getElementById`. / export const MEET_POPUP_CLASS = 'meet-popup'; /** Вкладки карточки в порядке показа.
