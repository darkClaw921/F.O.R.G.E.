<!--
  Шаблон README для tap-репозитория darkClaw921/homebrew-devforge.

  Использование (см. docs/homebrew-tap-setup.md, шаг 3):
      cp docs/homebrew-tap-README-template.md \
         ~/projects/homebrew-devforge/README.md

  В готовом tap-репо этот файл становится корневым README.md и виден на
  https://github.com/darkClaw921/homebrew-devforge.
-->

# Homebrew Tap for `devforge` (F.O.R.G.E.)

Homebrew-tap для проекта [**F.O.R.G.E.**](https://github.com/darkClaw921/F.O.R.G.E.) —
*Flow Orchestration and Real-time Governance Engine*. Это веб-«кокпит» поверх `tmux` +
kanban-доски + git-вкладки (через `lazygit`), упакованный в один Rust-бинарь `devforge`.

Сам код проекта живёт в [`darkClaw921/F.O.R.G.E.`](https://github.com/darkClaw921/F.O.R.G.E.).
Этот репозиторий содержит только Homebrew-формулу, которая собирает и устанавливает `devforge`.

---

## Что внутри

```
homebrew-devforge/
├── Formula/
│   └── devforge.rb     # формула, собирает devforge из source через cargo
├── LICENSE             # MIT (тот же, что в основном репо)
└── README.md           # этот файл
```

---

## Установка

```bash
brew tap darkClaw921/devforge
brew install devforge
```

После установки запустите:

```bash
devforge
```

Веб-UI откроется на `http://localhost:3000`.

> Имя tap (`darkClaw921/devforge`) — это сокращение от полного имени репо
> `darkClaw921/homebrew-devforge`: префикс `homebrew-` brew подставляет автоматически.

---

## Обновление

```bash
brew update
brew upgrade devforge
```

---

## Деинсталляция

Удалить только бинарь:

```bash
brew uninstall devforge
```

Удалить вместе с tap (если больше не планируете ставить):

```bash
brew uninstall devforge
brew untap darkClaw921/devforge
```

---

## Зависимости

`devforge` собирается из исходников Rust и тянет следующее окружение:

| Тип           | Пакет     | Зачем                                                    |
|---------------|-----------|----------------------------------------------------------|
| build         | `rust`    | Сборка `devforge` через `cargo install` (только на время компиляции). |
| runtime       | `tmux`    | Бэкенд для terminal-вкладки. Без `tmux` UI запустится, но terminal-сессии не будут работать. |
| опционально   | `lazygit` | Расширенный UI git-вкладки. Если не установлен — вкладка покажет fallback. |
| опционально   | `br` / `beads` | Интеграция с issue tracker (для kanban-доски). |

Build-зависимость `rust` Homebrew поставит автоматически на время сборки и удалит после неё (формула объявлена через `depends_on "rust" => :build`).

---

## Информация для мейнтейнеров

При выходе новой версии `vX.Y.Z` в основном репо F.O.R.G.E.:

1. В основном репо создать и запушить тег:
   ```bash
   git tag vX.Y.Z && git push origin vX.Y.Z
   SHA=$(git rev-parse vX.Y.Z)
   ```
2. Обновить в `Formula/devforge.rb` (как в основном репо, так и в этом tap-репо):
   - `tag:      "vX.Y.Z"`
   - `revision: "<значение $SHA>"`
3. Закоммитить и запушить изменение в этот tap-репо:
   ```bash
   git add Formula/devforge.rb
   git commit -m "devforge X.Y.Z"
   git push
   ```

Полные инструкции — в основном репо: [`docs/homebrew-tap-setup.md`](https://github.com/darkClaw921/F.O.R.G.E./blob/master/docs/homebrew-tap-setup.md).

---

## Лицензия

MIT — см. [`LICENSE`](./LICENSE). Совпадает с лицензией основного проекта
[`darkClaw921/F.O.R.G.E.`](https://github.com/darkClaw921/F.O.R.G.E.).
