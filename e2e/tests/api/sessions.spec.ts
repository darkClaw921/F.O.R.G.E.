import { test, expect } from '@playwright/test';
import {
  apiListSessions,
  apiCreateSession,
  apiDeleteSession,
  apiRenameSession,
  getActiveProjectPrefix,
  fullSessionName,
  e2ePrefix,
} from '../../fixtures/api-client';
import { cleanupE2ESessions } from '../../fixtures/tmux-helpers';

test.describe.configure({ mode: 'serial' });

const PREFIX = e2ePrefix();
let tmuxPrefix = '';

test.afterAll(async ({ request }) => {
  await cleanupE2ESessions(request, `${tmuxPrefix}-${PREFIX}`);
  // Also try plain prefix in case tmuxPrefix was empty
  await cleanupE2ESessions(request, PREFIX);
});

test.describe('Sessions API', () => {
  test('resolve active project prefix', async ({ request }) => {
    tmuxPrefix = await getActiveProjectPrefix(request);
    // Just a setup step — always passes
    expect(typeof tmuxPrefix).toBe('string');
  });

  test('GET /api/sessions returns an array', async ({ request }) => {
    const sessions = await apiListSessions(request);
    expect(Array.isArray(sessions)).toBe(true);
  });

  test('GET /api/sessions items have expected shape', async ({ request }) => {
    const name = `${PREFIX}shape`;
    await apiCreateSession(request, name);
    const fullName = fullSessionName(tmuxPrefix, name);
    try {
      const sessions = await apiListSessions(request);
      const match = sessions.find((s) => s.name === fullName);
      expect(match).toBeDefined();
      expect(typeof match!.name).toBe('string');
      expect(typeof match!.windows).toBe('number');
      expect(typeof match!.attached).toBe('number');
      expect(typeof match!.created).toBe('number');
      expect(typeof match!.origin).toBe('string');
    } finally {
      try { await apiDeleteSession(request, fullName); } catch { /* ignore */ }
    }
  });

  test('POST /api/sessions creates a real tmux session', async ({ request }) => {
    const name = `${PREFIX}create`;
    await apiCreateSession(request, name);
    const fullName = fullSessionName(tmuxPrefix, name);

    const sessions = await apiListSessions(request);
    const found = sessions.find((s) => s.name === fullName);
    expect(found).toBeDefined();

    await apiDeleteSession(request, fullName);
  });

  test('POST /api/sessions → 201 status code', async ({ request }) => {
    const name = `${PREFIX}status`;
    const resp = await request.post('http://127.0.0.1:17331/api/sessions', {
      data: { name },
    });
    expect(resp.status()).toBe(201);
    const fullName = fullSessionName(tmuxPrefix, name);
    await apiDeleteSession(request, fullName);
  });

  test('DELETE /api/sessions/:name removes the session', async ({ request }) => {
    const name = `${PREFIX}delete`;
    await apiCreateSession(request, name);
    const fullName = fullSessionName(tmuxPrefix, name);

    await apiDeleteSession(request, fullName);

    const sessions = await apiListSessions(request);
    const found = sessions.find((s) => s.name === fullName);
    expect(found).toBeUndefined();
  });

  test('DELETE /api/sessions/:name → 204 status code', async ({ request }) => {
    const name = `${PREFIX}deletestatus`;
    await apiCreateSession(request, name);
    const fullName = fullSessionName(tmuxPrefix, name);

    const resp = await request.delete(
      `http://127.0.0.1:17331/api/sessions/${encodeURIComponent(fullName)}`,
    );
    expect(resp.status()).toBe(204);
  });

  test('PATCH /api/sessions/:name renames the session', async ({ request }) => {
    const oldName = `${PREFIX}renameold`;
    const newName = `${PREFIX}renamenew`;
    await apiCreateSession(request, oldName);
    const fullOld = fullSessionName(tmuxPrefix, oldName);

    try {
      const result = await apiRenameSession(request, fullOld, newName);
      // Server returns the new name (possibly with prefix applied again)
      expect(result.name).toContain('renamenew');

      const sessions = await apiListSessions(request);
      const oldFound = sessions.find((s) => s.name === fullOld);
      expect(oldFound).toBeUndefined();
    } finally {
      // Clean up: try all possible names the server may have assigned
      for (const n of [
        `${tmuxPrefix}-${newName}`,
        newName,
        fullSessionName(tmuxPrefix, newName),
        `${tmuxPrefix}-${PREFIX}renamenew`,
      ]) {
        try { await apiDeleteSession(request, n); } catch { /* ignore */ }
      }
    }
  });

  test('PATCH /api/sessions/:name → 400 for unknown session', async ({ request }) => {
    const resp = await request.patch(
      'http://127.0.0.1:17331/api/sessions/no-such-session-xyz',
      { data: { name: 'whatever' } },
    );
    expect(resp.status()).toBe(400);
  });

  test('DELETE /api/sessions/:name → 400 for unknown session', async ({ request }) => {
    const resp = await request.delete(
      'http://127.0.0.1:17331/api/sessions/no-such-session-xyz',
    );
    expect(resp.status()).toBe(400);
  });

  test('POST /api/sessions → 400 on duplicate name', async ({ request }) => {
    const name = `${PREFIX}dup`;
    await apiCreateSession(request, name);
    const fullName = fullSessionName(tmuxPrefix, name);
    try {
      // Try creating again with the full prefixed name (already exists)
      const resp = await request.post('http://127.0.0.1:17331/api/sessions', {
        data: { name: fullName },
      });
      expect(resp.status()).toBe(400);
    } finally {
      await apiDeleteSession(request, fullName);
    }
  });
});
