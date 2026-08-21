# blocker_hint

plugins/meet/src/deps.rs: код нехватки (cloud_not_configured, whisper_unavailable, model_unavailable, metal_unavailable, ffmpeg_unavailable, claude_cli_unavailable, summary_disabled) → совет ДЕЙСТВИЕМ на русском. Доходит до человека в теле 409 (MeetError::DepUnavailable::message), в тексте события отката (StagePlan::fallback_note) и в ошибке стадии пайплайна. Фронтовой близнец — BLOCKER_LABELS в tmux-web/static/js/settings/meet-tab.js.
