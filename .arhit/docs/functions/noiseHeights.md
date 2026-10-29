# noiseHeights

`tmux-web/static/js/messenger/waveform-core.js` — высоты столбиков ЗАГЛУШКИ в пикселях.

Сигнатура: `noiseHeights(seed) -> Array<number>` длиной `WAVE_BARS`.

Генератор — xorshift32 от соли (`noiseSeed`): три сдвига и XOR на каждый столбик.
Взят именно он, а не `Math.random`, ровно ради воспроизводимости: одна и та же карточка
всегда даёт одну и ту же картинку, разные — разные. Высота — `WAVE_MIN_PX + (s % WAVE_RANGE_PX)`.

Заглушка живёт до первой настоящей огибающей и исчезает сама, как только та доедет.
