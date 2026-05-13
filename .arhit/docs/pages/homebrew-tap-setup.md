# Homebrew Tap Setup (Phase 3)

Артефакты Phase 3 (forge-xvvm) — подготовка к публикации Homebrew tap-репозитория darkClaw921/homebrew-devforge. Никакие git-операции (tag/push/commit/создание GitHub-репо) не выполняются агентом — вся ручная часть описана как чеклист для пользователя.

## Файлы

- docs/homebrew-tap-setup.md — полный пошаговый чеклист:
  * Шаг 1: создание GitHub-репо darkClaw921/homebrew-devforge (Public, без README/license/.gitignore, имя строго homebrew-devforge — обязательный префикс для brew tap).
  * Шаг 2: копирование MIT LICENSE из основного репо.
  * Шаг 3: локальная инициализация tap (git init; mkdir Formula; cp F.O.R.G.E./Formula/devforge.rb Formula/; cp docs/homebrew-tap-README-template.md README.md). Brew поддерживает формулы как в корне, так и в Formula/ — используем Formula/ для совместимости с homebrew-core.
  * Шаг 4 ('Релиз v0.1.0'): git tag v0.1.0; git push origin v0.1.0; SHA=$(git rev-parse v0.1.0); подстановка SHA в Formula/devforge.rb (revision:); локальная проверка brew install --build-from-source.
  * Шаг 5: публикация tap (cp синхронизированной формулы; git commit; git push).
  * Шаг 6: локальная верификация brew untap → brew tap → brew install devforge → brew audit --strict → brew test.
  * Шаг 7: процесс обновления формулы при будущих релизах vX.Y.Z.
  * Шаг 8: деинсталляция (brew uninstall + brew untap).
  * Шпаргалка с одной командой на каждое действие.

- docs/homebrew-tap-README-template.md — шаблон README для tap-репо (пользователь копирует в корень homebrew-devforge при инициализации):
  * Title 'Homebrew Tap for devforge (F.O.R.G.E.)' + описание со ссылкой на основной репо.
  * Структура tap (Formula/devforge.rb + LICENSE + README.md).
  * Команды установки: brew tap darkClaw921/devforge && brew install devforge.
  * Обновление: brew update && brew upgrade devforge.
  * Деинсталляция: brew uninstall devforge + brew untap darkClaw921/devforge.
  * Таблица зависимостей (build:rust, runtime:tmux, optional:lazygit, br/beads).
  * Mainainer-секция с релиз-процессом (ссылка на docs/homebrew-tap-setup.md).
  * MIT license (тот же, что в основном репо).

- Formula/devforge.rb — обновлён header-комментарий 'ACTION REQUIRED' с пошаговыми bash-командами для подстановки SHA. Поле revision: содержит явный placeholder 'REPLACE_WITH_v0.1.0_COMMIT_SHA' (вместо предыдущего 'PLACEHOLDER_FILL_IN_PHASE_3'). Ruby-синтаксис подтверждён (ruby -c → Syntax OK).

## Имя tap-репо

brew tap darkClaw921/devforge ищет github.com/darkClaw921/homebrew-devforge — префикс homebrew- обязателен (требование brew), brew автоматически его подставляет и lower-case-ит остаток. Поэтому имя репо homebrew-devforge соответствует tap-shortcut darkClaw921/devforge.

## Что делает пользователь руками

1. Создаёт GitHub-репо darkClaw921/homebrew-devforge (Public).
2. В основном F.O.R.G.E.: git tag v0.1.0 && git push origin v0.1.0; git rev-parse v0.1.0 → вставляет SHA в Formula/devforge.rb (поле revision:).
3. Клонит tap-репо локально, копирует Formula/devforge.rb + README + LICENSE, commit + push.
4. Проверяет: brew tap darkClaw921/devforge && brew install devforge && devforge --help.

## Что НЕ делает агент

git add, git commit, git tag, git push, gh repo create — всё это явно вынесено в чеклист docs/homebrew-tap-setup.md и выполняется пользователем.

## Связи

- [[homebrew-formula]] — содержимое самой формулы Formula/devforge.rb (Phase 2).
- main — реализация --help флага, нужного для brew test do (Phase 1).