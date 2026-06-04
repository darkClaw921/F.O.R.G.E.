# buildSessionItem

tmux-web/static/js/sessions/sessions.js — строит <li.session-item> для сайдбара. Классы: active (текущая сессия), needs-attention (s.needs_attention), has-next-step (если state.nextSteps[s.name] есть — голубое cyan-свечение, отдельный индикатор от ✶ claude-spark/is_generating). Содержит session-meta (name+sub), session-actions (rename/kill), при is_generating — span.claude-spark ✶ с tooltip. Клик по li → openSession.
