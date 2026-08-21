# waveHeights

`tmux-web/static/js/messenger/waveform-core.js` — высоты столбиков карточки: настоящая огибающая, если она есть, иначе шум.

Сигнатура: `waveHeights(waveform, seed) -> Array<number>`. Ровно две строки:
`realHeights(waveform)`, и если пусто — `noiseHeights(seed)`.

# Почему это отдельная ЧИСТАЯ функция

На ней держатся тесты рендера дорожки: проверять массив высот честнее и устойчивее, чем
ходить по узлам DOM. Панели зовут её не напрямую, а через `buildWave`, но тест —
напрямую.
