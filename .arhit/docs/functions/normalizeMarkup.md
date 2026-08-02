# normalizeMarkup

tmux-web/static/js/telegram/botkb.js — нормализация MessageDto.reply_markup.

Сигнатура: normalizeMarkup(markup) -> ?{kind: 'inline'|'keyboard'|'hide'|'force_reply',
rows: Button[][], resize, singleUse, persistent, placeholder: ?string}. null означает
«разметки нет» (поле отсутствует, не объект, неизвестный kind, все кнопки отбракованы).

У hide и force_reply rows ВСЕГДА пустой массив — по контракту с сервером.

ЗАЩИТА ОТ «КЛАВИАТУРЫ-БОМБЫ»: применяются потолки MAX_ROWS = 32 и MAX_ROW_BUTTONS = 12.
Разметка пришла от чужого бота через WS, доверять её размеру нельзя: без потолка десять тысяч
рядов подвесили бы вкладку прямо на рендере. Пустые ряды (все кнопки отбракованы) выбрасываются.
placeholder обрезается до PLACEHOLDER_MAX = 64.
