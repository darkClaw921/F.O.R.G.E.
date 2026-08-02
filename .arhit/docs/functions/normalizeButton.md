# normalizeButton

tmux-web/static/js/telegram/botkb.js — нормализация ReplyButtonDto в форму, пригодную для
рисования.

Сигнатура: normalizeButton(btn) -> ?{text, kind, data: ?string, url: ?string, query: ?string,
requiresPassword: boolean}. null означает «кнопку рисовать нельзя» (нет подписи, не объект).

Подпись прогоняется через cleanButtonText(.., BUTTON_TEXT_MAX); kind сверяется со списком
контракта dto.rs::ReplyButtonDto.kind, незнакомый становится 'other'. Принимает и snake_case
(requires_password — как приходит с сервера), и camelCase.

Вызывается из normalizeMarkup для каждой кнопки каждого ряда.
