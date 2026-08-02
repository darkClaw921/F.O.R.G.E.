# setTgContactsOpen

Открывает или закрывает off-canvas-колонку контактов: класс на #telegram, hidden у подложки #tg-contacts-scrim, aria-expanded у ☰. При открытии переносит фокус в колонку (у <aside> для этого стоит tabindex=-1), иначе клавиатурная навигация осталась бы в titlebar за пределами выехавшей панели.
