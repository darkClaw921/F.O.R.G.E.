# tmux-web/static/js/telegram/state.js

# js/telegram/state.js — клиентское состояние вкладки Telegram

Единственный мутируемый объект модуля (по образцу js/core/state.js):
импортируется остальными модулями плагина и правится ими напрямую. Никакой
реактивности — перерисовку инициирует тот, кто поменял поле.

## Инвариант идентификаторов

Все id — СТРОКИ, ровно как их отдаёт бэкенд: 64-битные идентификаторы Telegram
не помещаются в Number без потери точности, поэтому сравнивать их и
использовать как ключи Map можно только строками. `message_id` — число (в
Telegram он 32-битный).

## Поля tgState

- `dialogs: DialogDto[]` — список как пришёл с сервера;
- `messages: Map<dialogId, MessageDto[]>` — по возрастанию id;
- `activeDialogId: ?string`;
- `uploads: Map<uploadId, {file, sent, total, phase}>` — прогресс отправки;
- `mediaCache: Map<key, objectURL>` — при смене диалога URL'ы обязательно
  revoke'аются;
- `unreadTotal: number` — источник бейджа на кнопке ✈;
- `themeMode: 'auto'|'dark'|'light'` — режим темы панели (js/telegram/theme.js);
- `enabled: boolean` — плагин включён на сервере (GET /api/telegram/status = 200);
- `loginState: string` — последнее известное LoginStateDto.state;
- `selfUser: ?TgUserDto`;
- `ws: ?WebSocket`;
- `lat: ?number` — сглаженная задержка для LAT в статусбаре.

## Функции

- `clearTgAccountData()` — выбрасывает данные аккаунта: диалоги, сообщения,
  загрузки, медиа. objectURL'ы ОБЯЗАТЕЛЬНО revoke'аются — иначе скачанные
  бинарники живут в памяти страницы до перезагрузки, и после logout данные
  прошлого аккаунта остаются доступны по ссылке. Поля логина НЕ трогаются: их
  выставляет applyLoginState, который эту функцию и вызывает.
- `resetTgState()` — clearTgAccountData + сброс loginState/selfUser/lat (logout).
