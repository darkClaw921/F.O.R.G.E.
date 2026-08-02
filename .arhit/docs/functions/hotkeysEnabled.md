# hotkeysEnabled

keynav.js — включена ли клавиатурная навигация. Читает window.ForgeApp.state.userSettings лениво в момент нажатия (как hotkeys.js: модуль грузится раньше ответа /api/user-settings, и снимок на старте застыл бы навсегда). Сравнение '!== false', а НЕ '=== true': дефолт настройки telegram_hotkeys_enabled — ВКЛЮЧЕНО, поэтому отсутствие поля (старый файл настроек, backend не ответил) обязано означать 'работают'.
