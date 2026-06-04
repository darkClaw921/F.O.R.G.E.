# sendNextStep

tmux-web/static/js/echo/api.js — POST /api/echo/next-steps/:session/send {text}. Доставляет шаг в терминал tmux-сессии; если text пуст, бэкенд берёт сохранённый content. Рядом: listNextSteps (GET список предложений), feedbackNextStep(session,correction) → POST .../feedback (правило-коррекция в next_step_rules), dismissNextStep(session) → POST .../dismiss (снять без действия). Все шлют broadcast NextStepEvent{has_suggestion:false} после успеха.
