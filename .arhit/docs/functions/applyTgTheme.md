# applyTgTheme

Применяет тему панели Telegram: считает итог (format.js::resolveTgTheme) и ставит атрибут data-tg-theme на #telegram, а на кнопке ◐ обновляет data-tg-theme-mode, title и aria-label. Идемпотентна и дешёвая: вызывается на каждом входе на вкладку и на каждой мутации style у <html>, поэтому DOM трогается только при реальном изменении. Возвращает 'dark' | 'light'.
