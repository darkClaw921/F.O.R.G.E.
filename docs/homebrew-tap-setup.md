# Публикация devforge через Homebrew tap

`devforge` публикуется через **общий tap** `darkClaw921/homebrew-tap` (тот же, в котором уже живут другие формулы автора, например `arhit`). Отдельный репозиторий `homebrew-devforge` **не нужен** — это упрощает поддержку и позволяет переиспользовать существующий tap.

Команды конечного пользователя:

```bash
brew tap darkClaw921/tap
brew install devforge
```

---

## Архитектура

```
darkClaw921/F.O.R.G.E.            (этот репозиторий — исходники)
├── tmux-web/                     # Rust-проект (пакет devforge)
├── Formula/devforge.rb           # каноническая формула; источник правды
└── release.sh                    # автоматизация релиза

darkClaw921/homebrew-tap          (общий tap-репозиторий)
└── Formula/
    ├── arhit.rb
    └── devforge.rb               # копия из F.O.R.G.E./Formula/, обновляется release.sh
```

Идея: формула в основном репозитории — source of truth. При релизе `release.sh` обновляет в ней `url` (новый tag) и `sha256` (новый tarball), затем копирует обновлённый файл в tap-репо и пушит.

---

## Релизный workflow

### Однократный bootstrap (только для первой публикации)

Нужно один раз положить файл `Formula/devforge.rb` в tap-репо вручную, чтобы у release.sh было что обновлять. Шаги:

1. Локально клонировать tap:
   ```bash
   git clone git@github.com:darkClaw921/homebrew-tap.git ~/projects/homebrew-tap
   ```
2. Скопировать формулу:
   ```bash
   mkdir -p ~/projects/homebrew-tap/Formula
   cp Formula/devforge.rb ~/projects/homebrew-tap/Formula/devforge.rb
   ```
3. В скопированной формуле временно заменить `url` на актуальный тег (например `v0.1.1`), а `sha256` — на реальный (см. ниже как считать).
4. Закоммитить и запушить:
   ```bash
   cd ~/projects/homebrew-tap
   git add Formula/devforge.rb
   git commit -m "Add devforge formula"
   git push origin main
   ```

После этого все последующие релизы делает `release.sh` без ручных шагов.

### Регулярный релиз

```bash
./release.sh 0.1.2
```

Что делает скрипт:

1. Бампает `version` в `tmux-web/Cargo.toml`.
2. Обновляет `url` в `Formula/devforge.rb` на `archive/refs/tags/v0.1.2.tar.gz`.
3. Запускает `cargo build --release` как smoke-test.
4. `git add` → `git commit -m "Release v0.1.2"` → `git tag v0.1.2` → `git push origin master --tags`.
5. Качает с GitHub tarball нового релиза, считает `sha256`.
6. Подставляет sha256 обратно в локальный `Formula/devforge.rb`.
7. Клонирует tap-репо во временную директорию, копирует туда обновлённую формулу, коммитит и пушит в `main`.
8. Удаляет временную директорию.

Подсчёт sha256 вручную (если нужно):

```bash
curl -fsSL https://github.com/darkClaw921/F.O.R.G.E./archive/refs/tags/v0.1.1.tar.gz \
  | shasum -a 256 | awk '{print $1}'
```

---

## Локальная проверка без публикации

Установить из текущей рабочей копии (без тега, без tap-репо):

```bash
brew install --build-from-source --HEAD ./Formula/devforge.rb
devforge --help
```

Проверить, что формула пройдёт `brew audit`:

```bash
brew audit --strict --formula ./Formula/devforge.rb
```

После публикации в tap — полный пользовательский сценарий:

```bash
brew untap darkClaw921/tap 2>/dev/null || true
brew tap darkClaw921/tap
brew install devforge
which devforge          # /opt/homebrew/bin/devforge или /usr/local/bin/devforge
devforge --help
brew test devforge      # запустит test do ... end из формулы
```

---

## Откат

Если что-то сломалось:

```bash
# 1) Откатить тег в основном репо
git tag -d vX.Y.Z
git push origin :refs/tags/vX.Y.Z

# 2) Откатить формулу в tap-репо
cd ~/projects/homebrew-tap
git revert <hash-коммита-обновления-devforge>
git push origin main
```

GitHub-релиз (если был создан автоматически из тега) — удалить вручную через UI или `gh release delete vX.Y.Z`.

---

## Шпаргалка

| Действие                              | Команда                                                           |
|---------------------------------------|-------------------------------------------------------------------|
| Установить tap                        | `brew tap darkClaw921/tap`                                        |
| Установить devforge                   | `brew install devforge`                                           |
| Обновить                              | `brew update && brew upgrade devforge`                            |
| Удалить                               | `brew uninstall devforge`                                         |
| Отключить tap                         | `brew untap darkClaw921/tap`                                      |
| Релиз новой версии                    | `./release.sh X.Y.Z`                                              |
| Локальная установка (без релиза)      | `brew install --build-from-source --HEAD ./Formula/devforge.rb`   |
| Адрес tap-репо                        | `https://github.com/darkClaw921/homebrew-tap`                     |
