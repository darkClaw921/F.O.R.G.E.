// tmux-web — public window.ForgeApp contract (Phase 1).
//
// 1:1 копия публичного API из IIFE `tmux-web/static/app.js:6738-6742`:
//   window.ForgeApp = { sendToActivePty, state }
//
// На этот контракт завязан `tmux-web/static/quick-cmd.js:216-221, 585-591`
// (legacy IIFE-консумер). Изменять контракт НЕЛЬЗЯ без обновления quick-cmd.js.

import { state } from './core/state.js';
import { sendToActivePty } from './tabs/tui-tabs.js';
import { showDailySummary, hideDailySummary } from './daily-summary/daily-summary.js';

window.ForgeApp = {
    sendToActivePty: sendToActivePty,
    state: state,
    // Сводка дня — вью открывается извне (например, кнопкой в настройках,
    // Phase 5). ForgeApp.showDailySummary(day?) / hideDailySummary().
    showDailySummary: showDailySummary,
    hideDailySummary: hideDailySummary,
};
