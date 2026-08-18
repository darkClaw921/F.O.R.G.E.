# pressMeetButton

Общая ветка хоткея Shift+M (messenger/meet-row.js): находит в строке кнопку встречи через meetButtonOf и нажимает её. Возвращает false, когда кнопки нет — курсора нет, ссылки в сообщении нет или плагин выключен. Ложное true съело бы клавишу у остального интерфейса, поэтому событие не поглощается. Зовётся из telegram/keynav.js и vk/keynav.js (openMeetForCursor); у Битрикса ту же работу делает его собственный rowActionButton по тому же атрибуту data-action.
