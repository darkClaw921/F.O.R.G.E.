# Авто-апрув prompt'ов Claude Code (tw-pg5)

Фоновый воркер F.O.R.G.E., который сам подтверждает интерактивные запросы Claude Code в tmux-сессиях, чтобы агент не простаивал на «Do you want to proceed?».

## Состав изменения
- tmux-web/src/auto_approve.rs — новый модуль: classify_prompt, decide, DEFAULT_DANGEROUS, воркер run(AppState), 12 юнит-тестов.
- tmux-web/src/tmux.rs — send_key (одиночная клавиша без Enter) + вынесенный resolve_target; send_keys теперь использует resolve_target.
- tmux-web/src/attention.rs — normalize_ws стала pub(crate) (переиспользуется классификатором, копия не заводилась).
- tmux-web/src/user_settings.rs — 4 поля + PATCH-DTO + sanitize_blocklist.
- tmux-web/src/main.rs — mod auto_approve + tokio::spawn(auto_approve::run(app_state.clone())).
- tmux-web/static/js/settings/interface-tab.js — fieldset «Интерфейс» дополнен тумблером авто-апрува, чекбоксами broad/questions и textarea стоп-слов; зависимые контролы disabled, пока выключен главный.

## Формы prompt'ов
Permission3 (Yes / Yes-and-dont-ask / No), Permission2 (Yes / No), AllowDeny (Claude in Chrome: Allow / Deny), AllowBroad (Allow / Allow all actions on <host> for this session), Question (AskUserQuestion), Plan (ExitPlanMode).

Broad-режим жмёт «2» ТОЛЬКО в Permission3 и AllowBroad — там второй пункт расширяет разрешение. В Permission2 и AllowDeny второй пункт — отказ, там всегда «1».

План (ExitPlanMode) не подтверждается автоматически никогда.

## Блоклист
Встроенный DEFAULT_DANGEROUS (rm -rf, git push --force, git reset --hard, drop table/database, dd if=, mkfs, chmod 777, | sh, | bash, shutdown …) плюс пользовательские стоп-слова из настроек. Совпадение по нормализованной панели в нижнем регистре → prompt пропускается, свечение продолжает гореть.

## Защиты от ложных нажатий
- Гард стабильности: тот же hash_pane два тика подряд.
- Анти-луп: не более 2 нажатий за серию «prompt непрерывно распознан»; счётчик обнуляется по исчезновению prompt'а (classify_prompt == None), а не по смене хэша — нажатая цифра сама печатается на экран и меняет хэш.

## Как проверялось
Юнит-тесты (12) + живая проверка на ИЗОЛИРОВАННОМ tmux-сервере (env TMUX_TMPDIR=/private/tmp/aa-verify, unset TMUX — иначе команды уходят на общий сервер и трогают рабочие сессии) с HOME во временной папке:
1) фикстура Permission2 → в логе два auto-approved (attempt=1, затем attempt=2 с Enter), дальше молчание — анти-луп держит;
2) фикстура с rm -rf и фикстура плана → 0 нажатий, в debug-логе «auto-approve skipped»;
3) auto_approve_enabled=false → 0 нажатий и 0 обращений к tmux.

Именно живая проверка выявила, что счётчик попыток нельзя вешать на хэш панели.