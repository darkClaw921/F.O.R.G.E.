/**
 * Helpers for tmux session lifecycle in E2E tests.
 *
 * Tests create sessions with a unique `e2e_<ts>_` prefix so they never
 * touch the developer's own sessions. Cleanup is done via the devforge API
 * (DELETE /api/sessions/:name), falling back to direct `tmux kill-session`
 * for leftover sessions after failures.
 */
import type { APIRequestContext } from '@playwright/test';
import { execSync } from 'child_process';
import { BASE_URL } from './api-client';

/**
 * Kill all tmux sessions whose names start with `prefix` via the API.
 * Falls back to direct `tmux kill-session` if API call fails.
 */
export async function cleanupE2ESessions(
  request: APIRequestContext,
  prefix: string,
): Promise<void> {
  // List sessions via API (does not throw if tmux has no server)
  let sessions: any[] = [];
  try {
    const resp = await request.get(`${BASE_URL}/api/sessions`);
    if (resp.ok()) {
      sessions = await resp.json();
    }
  } catch {
    // Server not up, nothing to clean
    return;
  }

  for (const s of sessions) {
    if (typeof s.name === 'string' && s.name.startsWith(prefix)) {
      try {
        await request.delete(`${BASE_URL}/api/sessions/${encodeURIComponent(s.name)}`);
      } catch {
        // Fallback: kill via tmux CLI directly
        try {
          execSync(`tmux kill-session -t ${JSON.stringify(s.name)}`, { stdio: 'ignore' });
        } catch {
          // Already gone, that's fine
        }
      }
    }
  }
}

/**
 * Kill all tmux sessions starting with `prefix` directly via tmux CLI.
 * Used in global teardown when the API server is already shut down.
 */
export function cleanupE2ESessionsDirect(prefix: string): void {
  try {
    const raw = execSync('tmux list-sessions -F "#{session_name}"', {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    for (const name of raw.split('\n').map((l) => l.trim()).filter(Boolean)) {
      if (name.startsWith(prefix)) {
        try {
          execSync(`tmux kill-session -t ${JSON.stringify(name)}`, { stdio: 'ignore' });
        } catch {
          // Already gone
        }
      }
    }
  } catch {
    // tmux not running or not installed — nothing to clean
  }
}

/** Generate a unique tmux session name with the shared e2e prefix. */
export function uniqueSession(testTag: string): string {
  return `e2e_${Date.now()}_${testTag}`;
}
