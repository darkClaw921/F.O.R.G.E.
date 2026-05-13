# Создание tap-репозитория `darkClaw921/homebrew-devforge`

Этот документ — пошаговый чеклист для **ручных действий пользователя** при публикации Homebrew-tap для проекта F.O.R.G.E. (`devforge`). Агент не выполняет ни одной из перечисленных ниже git/GitHub команд — все шаги выполняет человек.

---

## 0. Предусловия

- Phase 1 (embedded static) и Phase 2 (Formula/devforge.rb) завершены.
- Файл `Formula/devforge.rb` существует в корне основного репо F.O.R.G.E. и собирается локально через
  `brew install --build-from-source ./Formula/devforge.rb` (или `--HEAD`, пока нет реального тега).
- Поле `revision:` в формуле сейчас содержит placeholder — его подменим на реальный SHA в разделе
  «Релиз v0.1.0» ниже.

---

## 1. Создать пустой GitHub-репозиторий

Имя репозитория **строго** обязательно: `homebrew-devforge` — префикс `homebrew-` требуется командой
`brew tap`. Owner: `darkClaw921`. Полный путь: `https://github.com/darkClaw921/homebrew-devforge`.

Откройте https://github.com/new и заполните:

| Поле          | Значение                                                              |
|---------------|-----------------------------------------------------------------------|
| Owner         | `darkClaw921`                                                         |
| Repository    | `homebrew-devforge`                                                   |
| Description   | `Homebrew tap for devforge (F.O.R.G.E.) — tmux/kanban/git web cockpit`|
| Visibility    | **Public** (private tap не работает без `HOMEBREW_GITHUB_API_TOKEN`)  |
| Initialize    | без README, без `.gitignore`, без license (создадим вручную ниже)     |

Нажмите **Create repository**.

> Имя `homebrew-devforge` ⇒ `brew tap darkClaw921/devforge` (brew автоматически вырезает префикс
> `homebrew-` и lower-case-ит остаток).

---

## 2. Скопировать лицензию из основного репо

В корне `homebrew-devforge` должна лежать `LICENSE` — копируем MIT из основного репо F.O.R.G.E.,
менять годы/имя не нужно (тот же владелец).

```bash
cp /path/to/F.O.R.G.E./LICENSE ~/projects/homebrew-devforge/LICENSE
```

Если в основном репо `LICENSE` ещё не создан — создайте его прежде (MIT с владельцем
`darkClaw921`).

---

## 3. Инициализировать tap локально

```bash
mkdir -p ~/projects/homebrew-devforge && cd ~/projects/homebrew-devforge
git init
git branch -M main

# Структура: brew поддерживает оба варианта — формула в корне или в Formula/.
# Используем Formula/ (как в homebrew-core) для совместимости и читаемости.
mkdir Formula
cp /path/to/F.O.R.G.E./Formula/devforge.rb Formula/devforge.rb

# README — скопировать шаблон из основного репо (docs/homebrew-tap-README-template.md)
cp /path/to/F.O.R.G.E./docs/homebrew-tap-README-template.md README.md

# LICENSE — см. шаг 2

git remote add origin git@github.com:darkClaw921/homebrew-devforge.git
```

Итоговая структура tap-репо:

```
homebrew-devforge/
├── Formula/
│   └── devforge.rb        # ← скопирован из F.O.R.G.E./Formula/devforge.rb
├── LICENSE                # ← MIT, тот же что в основном репо
└── README.md              # ← из docs/homebrew-tap-README-template.md
```

---

## 4. Релиз v0.1.0 (основной репо F.O.R.G.E.)

Эти команды выполняются в корне основного репо `F.O.R.G.E.`, **не** в tap-репо.

```bash
cd /path/to/F.O.R.G.E.

# 4.1. Создать тег
git tag v0.1.0
git push origin v0.1.0

# 4.2. Получить полный SHA коммита тега
SHA=$(git rev-parse v0.1.0)
echo "$SHA"
# пример вывода: 1a2b3c4d5e6f7890abcdef1234567890abcdef12

# 4.3. Подставить SHA в Formula/devforge.rb
#      Открыть файл и заменить строку
#          revision: "REPLACE_WITH_v0.1.0_COMMIT_SHA"
#      на
#          revision: "<вставить значение $SHA>"
#
#      Полный SHA предпочтительнее короткого — brew принимает оба, но 40-символьный
#      исключает риск коллизии и более устойчив к force-push на тег.

# 4.4. Проверить, что формула собирается с уже зафиксированным revision
brew uninstall --force devforge 2>/dev/null || true
brew install --build-from-source ./Formula/devforge.rb
devforge --help
```

> ВАЖНО: правка `revision:` в `Formula/devforge.rb` основного репо **не** требует отдельного
> коммита прямо сейчас — её можно зафиксировать вместе с любым следующим коммитом релиза.
> Главное — синхронизировать ту же версию файла в tap-репо (см. шаг 5).

---

## 5. Опубликовать tap-репозиторий

```bash
cd ~/projects/homebrew-devforge

# Синхронизировать формулу с реальным SHA (важно: копируем уже исправленную в шаге 4.3 версию)
cp /path/to/F.O.R.G.E./Formula/devforge.rb Formula/devforge.rb

git add Formula/devforge.rb README.md LICENSE
git commit -m "Initial tap with devforge v0.1.0"
git push -u origin main
```

---

## 6. Локальная проверка tap-сценария

Имитируем пользователя, который ставит devforge через brew tap:

```bash
# Если уже было — отвязать
brew untap darkClaw921/devforge 2>/dev/null || true

# Подключить tap
brew tap darkClaw921/devforge

# Установить
brew install devforge

# Проверить запуск
devforge --help
which devforge
# должен быть: /opt/homebrew/bin/devforge (Apple Silicon) или /usr/local/bin/devforge (Intel)
```

Дополнительные проверки качества формулы:

```bash
# Audit формулы (strict — как для homebrew-core)
brew audit --strict --tap darkClaw921/devforge devforge

# Test-блок (запустит секцию test do ... end из формулы)
brew test devforge
```

---

## 7. Обновление формулы при следующих релизах

При выходе версии `vX.Y.Z`:

1. В основном F.O.R.G.E.:
   ```bash
   git tag vX.Y.Z && git push origin vX.Y.Z
   SHA=$(git rev-parse vX.Y.Z)
   ```
2. Обновить `tag:` и `revision:` в `Formula/devforge.rb` (в обоих репо — основном и tap).
3. В tap-репо:
   ```bash
   cd ~/projects/homebrew-devforge
   cp /path/to/F.O.R.G.E./Formula/devforge.rb Formula/devforge.rb
   git add Formula/devforge.rb
   git commit -m "devforge X.Y.Z"
   git push
   ```
4. Пользователи обновляются:
   ```bash
   brew update && brew upgrade devforge
   ```

---

## 8. Отвязка / удаление tap (для пользователей)

```bash
brew uninstall devforge
brew untap darkClaw921/devforge
```

---

## Шпаргалка

| Действие                  | Команда                                                   |
|---------------------------|-----------------------------------------------------------|
| Подключить tap            | `brew tap darkClaw921/devforge`                           |
| Установить                | `brew install devforge`                                   |
| Обновить                  | `brew update && brew upgrade devforge`                    |
| Удалить                   | `brew uninstall devforge`                                 |
| Отключить tap             | `brew untap darkClaw921/devforge`                         |
| Полный путь репо          | `https://github.com/darkClaw921/homebrew-devforge`        |
| Имя формулы в репо        | `Formula/devforge.rb` (или `devforge.rb` в корне)         |
