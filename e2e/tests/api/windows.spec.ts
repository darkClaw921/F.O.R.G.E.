import { test, expect } from '@playwright/test';
import {
  apiCreateSession,
  apiDeleteSession,
  apiListWindows,
  apiCreateWindow,
  apiSelectWindow,
  apiDeleteWindow,
  apiRenameWindow,
  getActiveProjectPrefix,
  fullSessionName,
  e2ePrefix,
} from '../../fixtures/api-client';
import { cleanupE2ESessions } from '../../fixtures/tmux-helpers';

test.describe.configure({ mode: 'serial' });

const PREFIX = e2ePrefix();
let tmuxPrefix = '';
let SESSION = '';

test.beforeAll(async ({ request }) => {
  tmuxPrefix = await getActiveProjectPrefix(request);
  const baseName = `${PREFIX}windows`;
  await apiCreateSession(request, baseName);
  SESSION = fullSessionName(tmuxPrefix, baseName);
});

test.afterAll(async ({ request }) => {
  await cleanupE2ESessions(request, fullSessionName(tmuxPrefix, PREFIX));
  await cleanupE2ESessions(request, PREFIX);
});

test.describe('Windows API', () => {
  test('GET /api/sessions/:name/windows returns array of windows', async ({ request }) => {
    const windows = await apiListWindows(request, SESSION);
    expect(Array.isArray(windows)).toBe(true);
    // A fresh session always has at least 1 window
    expect(windows.length).toBeGreaterThanOrEqual(1);
  });

  test('window items have expected shape', async ({ request }) => {
    const windows = await apiListWindows(request, SESSION);
    const w = windows[0];
    expect(typeof w.index).toBe('number');
    expect(typeof w.name).toBe('string');
    expect(typeof w.active).toBe('boolean');
    expect(typeof w.panes).toBe('number');
  });

  test('POST /api/sessions/:name/windows → 201 and increases window count', async ({
    request,
  }) => {
    const before = await apiListWindows(request, SESSION);

    const resp = await request.post(
      `http://127.0.0.1:17331/api/sessions/${encodeURIComponent(SESSION)}/windows`,
      { data: {} },
    );
    expect(resp.status()).toBe(201);

    const after = await apiListWindows(request, SESSION);
    expect(after.length).toBeGreaterThan(before.length);
  });

  test('POST /api/sessions/:name/windows with name creates named window', async ({
    request,
  }) => {
    await apiCreateWindow(request, SESSION, 'e2e-named-win');
    const windows = await apiListWindows(request, SESSION);
    const found = windows.find((w) => w.name === 'e2e-named-win');
    expect(found).toBeDefined();
  });

  test('POST .../windows/:index/select → 204 and window becomes active', async ({
    request,
  }) => {
    const windows = await apiListWindows(request, SESSION);
    // Select the first window (index 0 is always present)
    const firstIdx = windows[0].index;
    await apiSelectWindow(request, SESSION, firstIdx);

    const updated = await apiListWindows(request, SESSION);
    const active = updated.find((w) => w.active);
    expect(active).toBeDefined();
    expect(active!.index).toBe(firstIdx);
  });

  test('PATCH .../windows/:index renames the window', async ({ request }) => {
    const windows = await apiListWindows(request, SESSION);
    const target = windows[0];
    const result = await apiRenameWindow(request, SESSION, target.index, 'renamed-win');
    expect(result.name).toBe('renamed-win');

    const updated = await apiListWindows(request, SESSION);
    const renamed = updated.find((w) => w.index === target.index);
    expect(renamed?.name).toBe('renamed-win');
  });

  test('PATCH .../windows/:index → 400 for empty name', async ({ request }) => {
    const windows = await apiListWindows(request, SESSION);
    const idx = windows[0].index;
    const resp = await request.patch(
      `http://127.0.0.1:17331/api/sessions/${encodeURIComponent(SESSION)}/windows/${idx}`,
      { data: { name: '   ' } },
    );
    expect(resp.status()).toBe(400);
  });

  test('DELETE .../windows/:index removes the window', async ({ request }) => {
    // First add a fresh window so we can safely delete it without
    // destroying the whole session (can't delete the last window)
    await apiCreateWindow(request, SESSION, 'to-delete');
    const before = await apiListWindows(request, SESSION);
    const toDelete = before.find((w) => w.name === 'to-delete');
    expect(toDelete).toBeDefined();

    await apiDeleteWindow(request, SESSION, toDelete!.index);

    const after = await apiListWindows(request, SESSION);
    const stillThere = after.find((w) => w.index === toDelete!.index);
    expect(stillThere).toBeUndefined();
  });

  test('GET .../windows on unknown session → 400', async ({ request }) => {
    const resp = await request.get(
      'http://127.0.0.1:17331/api/sessions/no-such-session-xyz/windows',
    );
    expect(resp.status()).toBe(400);
  });
});
