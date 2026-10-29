# tmux-web/tests/frontend/agent_stats_live.test.mjs

Node tests actual createAgentLive с минимальным DOM и управляемыми clock/timer/API. Проверяют четыре provider counts, состояния working/waiting/unknown, server elapsed и freeze unknown, отсутствие фальшивых нулей токенов/цен, keyed rows и сохранение focus/draft. Проверяют группировку linked pane aliases, точный sessionId при binding/unbinding, failure/retry controls, onChanged и cleanup ticker при hide/destroy. Нет production mock веток.
