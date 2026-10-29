# JoinStep

Что делать на текущем экране входа (plugins/meet/src/bot/chrome.rs). Решение вынесено чистой функцией next_join_step намеренно: вход — самая ветвистая часть адаптера, и проверять её прогонами браузера означало бы не проверять вовсе.

Варианты: ContinueInBrowser (интерстишал либо модалка, снимающая заслонку перед входом), TypeName (гостевая ветка), MuteDevices, Join, Lobby, Captcha (не решаем НИКОГДА — зовём человека), Denied, Ended, Inside, Wait.

MuteDevices и Ended появились в Ф13 из живого замера vcc.airlabs.pro — см. страницу forge-meet-phase13-vcc-bot.
