# tmux-web/static/css/modals.css

Универсальные стили модальных окон: .modal-overlay (position fixed inset 0, dim background, z-index 1000), .modal-card (центрированный card с border-radius, scrollable body), .modal-header/.modal-body/.modal-footer, .modal-actions (flex-end кнопок), .modal-actions .spacer (растягиватель). Также включает Task Modal (Phase 6.C) — формы создания/редактирования карточек kanban: .task-modal form-rows, label+input wrappers, textarea для description. 277 строк (964-1240 оригинала). НЕ включает kanban-карточные стили (1241+) — те в tasks.css.
