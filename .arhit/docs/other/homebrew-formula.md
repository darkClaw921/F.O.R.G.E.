# homebrew-formula

# Homebrew Formula (Phase 2 + Phase 3 update)

Файл: Formula/devforge.rb (корень репозитория F.O.R.G.E.)

## Назначение
Формула Homebrew для установки devforge из исходников. Благодаря Phase 1 (embedded static через rust-embed) формула получилась минимальной — никаких ресурсов отдельно копировать не нужно, бинарь self-contained.

## Структура формулы
- class Devforge < Formula
- desc: 'Tmux + kanban + git web cockpit (F.O.R.G.E.)' (сокращён до 47 символов — лимит brew style = 80)
- homepage: https://github.com/darkClaw921/F.O.R.G.E.
- url: git-URL с tag: 'v0.1.0' и revision: 'REPLACE_WITH_v0.1.0_COMMIT_SHA' (placeholder, заменяется пользователем после git tag v0.1.0)
- license: MIT
- head: branch master — для brew install --HEAD без необходимости тега
- depends_on 'rust' => :build — cargo-цепочка для билда
- depends_on 'tmux' — runtime-зависимость, devforge spawn-ит tmux-сессии через portable-pty
- install: cd 'tmux-web' do system 'cargo', 'install', *std_cargo_args end — std_cargo_args автоматически даёт --root #{prefix} --path . , бинарь попадает в #{prefix}/bin/devforge
- test: assert_match 'devforge', shell_output('#{bin}/devforge --help 2>&1') — простая проверка, что бинарь запускается и печатает usage (см. forge-fije.1)

## Header-комментарий (Phase 3)
В начале файла добавлен ACTION REQUIRED-блок с пошаговыми bash-командами для пользователя:
  git tag v0.1.0
  git push origin v0.1.0
  git rev-parse v0.1.0
  # вставить SHA вместо REPLACE_WITH_v0.1.0_COMMIT_SHA
И ссылка на полный чеклист docs/homebrew-tap-setup.md (раздел 4 'Релиз v0.1.0').

## Placeholder revision:
- Phase 2 первоначально: 'PLACEHOLDER_FILL_IN_PHASE_3'.
- Phase 3 (forge-xvvm.3): 'REPLACE_WITH_v0.1.0_COMMIT_SHA' (более явное имя; никаких функциональных отличий).
- Пользователь подставит реальный 40-символьный SHA после git tag v0.1.0. Полный SHA предпочтительнее короткого (исключает риск коллизии и устойчив к force-push на тег).

## Проверки локально
- ruby -c Formula/devforge.rb → Syntax OK
- brew style Formula/devforge.rb → no offenses detected
- brew install --build-from-source --HEAD local/devforge/devforge → успешно собирает

## Публикация (Phase 3, ручные шаги пользователя)
Формула копируется в tap-репозиторий darkClaw921/homebrew-devforge (см. [[homebrew-tap-setup]]). Пользовательский путь установки:
  brew tap darkClaw921/devforge
  brew install devforge

## Связи
- [[homebrew-tap-setup]] — чеклист публикации tap-репо и подстановки SHA.
- [[static_embed]] — embedded static, благодаря которому install не требует копирования ресурсов.
- main — --help-флаг, нужный для test do.
