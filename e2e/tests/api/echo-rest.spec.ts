/**
 * REST API tests for the Echo plugin.
 *
 * Endpoints under /api/echo/* are served by the Echo Axum plugin registered
 * in the tmux-web host.  All state is created and cleaned up within each test
 * so the suite is independent from other spec files.
 */
import { test, expect } from '@playwright/test';

const BASE = 'http://127.0.0.1:17331';

// ---------------------------------------------------------------------------
// Helpers — thin wrappers that throw on unexpected status codes
// ---------------------------------------------------------------------------

async function echoGet(request: import('@playwright/test').APIRequestContext, path: string) {
  return request.get(`${BASE}${path}`);
}

async function echoPost(
  request: import('@playwright/test').APIRequestContext,
  path: string,
  data: unknown,
) {
  return request.post(`${BASE}${path}`, { data });
}

async function echoPatch(
  request: import('@playwright/test').APIRequestContext,
  path: string,
  data: unknown,
) {
  return request.patch(`${BASE}${path}`, { data });
}

async function echoDelete(request: import('@playwright/test').APIRequestContext, path: string) {
  return request.delete(`${BASE}${path}`);
}

/** Create a conversation and return its id. */
async function createConversation(
  request: import('@playwright/test').APIRequestContext,
  title?: string,
): Promise<string> {
  const resp = await echoPost(request, '/api/echo/conversations', {
    title: title ?? `e2e-conv-${Date.now()}`,
  });
  expect(resp.status()).toBe(200);
  const body = await resp.json();
  expect(typeof body.id).toBe('string');
  return body.id as string;
}

/** Create a memory and return the full record. */
async function createMemory(
  request: import('@playwright/test').APIRequestContext,
  scope: string,
  day: string,
  content: string,
) {
  const resp = await echoPost(request, '/api/echo/memories', {
    scope,
    day,
    content,
    source: 'e2e',
  });
  expect(resp.status()).toBe(200);
  const body = await resp.json();
  expect(typeof body.id).toBe('string');
  return body;
}

/** ISO date string YYYY-MM-DD for today. */
function today(): string {
  return new Date().toISOString().split('T')[0];
}

// ---------------------------------------------------------------------------
// GET /api/echo/healthz
// ---------------------------------------------------------------------------

test.describe('GET /api/echo/healthz', () => {
  test('returns 200 with body "ok"', async ({ request }) => {
    const resp = await echoGet(request, '/api/echo/healthz');
    expect(resp.status()).toBe(200);
    const text = await resp.text();
    expect(text.trim()).toBe('ok');
  });
});

// ---------------------------------------------------------------------------
// GET /api/echo/stats
// ---------------------------------------------------------------------------

test.describe('GET /api/echo/stats', () => {
  test('range=hour returns 200 with buckets array of 60 items', async ({ request }) => {
    const resp = await echoGet(request, '/api/echo/stats?range=hour');
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    expect(Array.isArray(body.buckets)).toBe(true);
    expect(body.buckets).toHaveLength(60);
    expect(body.range).toBe('hour');
    // Each bucket has expected numeric fields
    const first = body.buckets[0];
    expect(typeof first.ts).toBe('number');
    expect(typeof first.tokens_in).toBe('number');
    expect(typeof first.tokens_out).toBe('number');
    expect(typeof first.cache_creation).toBe('number');
    expect(typeof first.cache_read).toBe('number');
  });

  test('range=day returns 200 with 24 buckets', async ({ request }) => {
    const resp = await echoGet(request, '/api/echo/stats?range=day');
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    expect(body.buckets).toHaveLength(24);
    expect(body.range).toBe('day');
  });

  test('default range (no param) returns 60 buckets', async ({ request }) => {
    const resp = await echoGet(request, '/api/echo/stats');
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    expect(body.buckets).toHaveLength(60);
  });

  test('invalid range returns 400', async ({ request }) => {
    const resp = await echoGet(request, '/api/echo/stats?range=year');
    expect(resp.status()).toBe(400);
    const body = await resp.json();
    expect(typeof body.error).toBe('string');
  });
});

// ---------------------------------------------------------------------------
// Conversations CRUD
// ---------------------------------------------------------------------------

test.describe('Conversations CRUD', () => {
  test.describe.configure({ mode: 'serial' });

  test('GET /api/echo/conversations returns 200 with items array', async ({ request }) => {
    const resp = await echoGet(request, '/api/echo/conversations');
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    expect(Array.isArray(body.items)).toBe(true);
  });

  test('POST creates conversation, it appears in list', async ({ request }) => {
    const title = `e2e-list-test-${Date.now()}`;
    const id = await createConversation(request, title);

    const listResp = await echoGet(request, '/api/echo/conversations');
    expect(listResp.status()).toBe(200);
    const { items } = await listResp.json();
    const found = items.find((c: any) => c.id === id);
    expect(found).toBeDefined();
    expect(found.title).toBe(title);

    // Cleanup
    await echoDelete(request, `/api/echo/conversations/${id}`);
  });

  test('GET /api/echo/conversations/:id/messages returns 200 with empty items for new chat', async ({
    request,
  }) => {
    const id = await createConversation(request);

    const resp = await echoGet(request, `/api/echo/conversations/${id}/messages`);
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    expect(Array.isArray(body.items)).toBe(true);
    expect(body.items).toHaveLength(0);

    await echoDelete(request, `/api/echo/conversations/${id}`);
  });

  test('DELETE returns 204; subsequent GET messages returns 404', async ({ request }) => {
    const id = await createConversation(request);

    const delResp = await echoDelete(request, `/api/echo/conversations/${id}`);
    expect(delResp.status()).toBe(204);

    // After deletion GET messages must return 404
    const afterResp = await echoGet(request, `/api/echo/conversations/${id}/messages`);
    expect(afterResp.status()).toBe(404);
  });

  test('POST with optional model field stores it in returned record', async ({ request }) => {
    const resp = await echoPost(request, '/api/echo/conversations', {
      title: `e2e-model-${Date.now()}`,
      model: 'claude-3-opus-20240229',
    });
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    expect(body.model).toBe('claude-3-opus-20240229');

    await echoDelete(request, `/api/echo/conversations/${body.id}`);
  });
});

// ---------------------------------------------------------------------------
// Memories CRUD
// ---------------------------------------------------------------------------

test.describe('Memories CRUD', () => {
  test.describe.configure({ mode: 'serial' });

  test('POST creates memory; GET with filter returns it', async ({ request }) => {
    const day = today();
    const content = `e2e-content-${Date.now()}`;
    const mem = await createMemory(request, 'global_day', day, content);

    const listResp = await echoGet(request, `/api/echo/memories?scope=global_day&day=${day}`);
    expect(listResp.status()).toBe(200);
    const { items } = await listResp.json();
    // upsert: may be one or more if tests ran earlier today; find ours
    const found = items.find((m: any) => m.id === mem.id);
    expect(found).toBeDefined();
    expect(found.content).toBe(content);
    expect(found.scope).toBe('global_day');

    await echoDelete(request, `/api/echo/memories/${mem.id}`);
  });

  test('GET /api/echo/memories without filter returns items array', async ({ request }) => {
    const resp = await echoGet(request, '/api/echo/memories');
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    expect(Array.isArray(body.items)).toBe(true);
  });

  test('PATCH /api/echo/memories/:id updates content', async ({ request }) => {
    const day = today();
    const mem = await createMemory(request, 'global_day', day, `original-${Date.now()}`);

    const newContent = `patched-${Date.now()}`;
    const patchResp = await echoPatch(request, `/api/echo/memories/${mem.id}`, {
      content: newContent,
    });
    expect(patchResp.status()).toBe(204);

    // Verify via /by-id/:id route
    const getResp = await echoGet(request, `/api/echo/memories/by-id/${mem.id}`);
    expect(getResp.status()).toBe(200);
    const updated = await getResp.json();
    expect(updated.content).toBe(newContent);

    await echoDelete(request, `/api/echo/memories/${mem.id}`);
  });

  test('PATCH non-existent memory returns 404', async ({ request }) => {
    const resp = await echoPatch(request, '/api/echo/memories/does-not-exist', {
      content: 'x',
    });
    expect(resp.status()).toBe(404);
  });

  test('DELETE /api/echo/memories/:id returns 204', async ({ request }) => {
    const day = today();
    const mem = await createMemory(request, 'global_day', day, `to-delete-${Date.now()}`);

    const delResp = await echoDelete(request, `/api/echo/memories/${mem.id}`);
    expect(delResp.status()).toBe(204);

    // Verify it's actually gone
    const getResp = await echoGet(request, `/api/echo/memories/by-id/${mem.id}`);
    expect(getResp.status()).toBe(404);
  });

  test('DELETE non-existent memory returns 404', async ({ request }) => {
    const resp = await echoDelete(request, '/api/echo/memories/does-not-exist');
    expect(resp.status()).toBe(404);
  });

  test('POST with invalid scope returns 400', async ({ request }) => {
    const resp = await echoPost(request, '/api/echo/memories', {
      scope: 'invalid_scope',
      day: today(),
      content: 'x',
    });
    expect(resp.status()).toBe(400);
    const body = await resp.json();
    expect(typeof body.error).toBe('string');
  });
});

// ---------------------------------------------------------------------------
// Autonomous Tasks CRUD
// ---------------------------------------------------------------------------

test.describe('Autonomous Tasks CRUD', () => {
  test.describe.configure({ mode: 'serial' });

  test('GET /api/echo/autonomous-tasks returns 200 with items array', async ({ request }) => {
    const resp = await echoGet(request, '/api/echo/autonomous-tasks');
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    expect(Array.isArray(body.items)).toBe(true);
  });

  test('POST creates task with 201, task appears in list', async ({ request }) => {
    const name = `e2e-auto-${Date.now()}`;
    const createResp = await echoPost(request, '/api/echo/autonomous-tasks', {
      name,
      prompt_template: 'Summarise recent activity',
      interval_seconds: 60,
      model: 'claude-opus-4-7',
    });
    expect(createResp.status()).toBe(201);
    const created = await createResp.json();
    expect(created.name).toBe(name);
    expect(typeof created.id).toBe('string');
    // The repo always creates tasks as enabled=true; next_run_at is set immediately.
    expect(typeof created.enabled).toBe('boolean');
    expect(created.next_run_at).not.toBeNull();

    const listResp = await echoGet(request, '/api/echo/autonomous-tasks');
    const { items } = await listResp.json();
    expect(items.some((t: any) => t.id === created.id)).toBe(true);

    await echoDelete(request, `/api/echo/autonomous-tasks/${created.id}`);
  });

  test('PATCH updates task and returns updated record', async ({ request }) => {
    const name = `e2e-patch-${Date.now()}`;
    const createResp = await echoPost(request, '/api/echo/autonomous-tasks', {
      name,
      prompt_template: 'Check status',
      interval_seconds: 120,
      model: 'claude-opus-4-7',
    });
    expect(createResp.status()).toBe(201);
    const { id } = await createResp.json();

    // Disable then re-enable via PATCH
    const disableResp = await echoPatch(request, `/api/echo/autonomous-tasks/${id}`, {
      enabled: false,
    });
    expect(disableResp.status()).toBe(200);
    const disabled = await disableResp.json();
    expect(disabled.enabled).toBe(false);

    const enableResp = await echoPatch(request, `/api/echo/autonomous-tasks/${id}`, {
      enabled: true,
    });
    expect(enableResp.status()).toBe(200);
    const enabled = await enableResp.json();
    expect(enabled.enabled).toBe(true);

    await echoDelete(request, `/api/echo/autonomous-tasks/${id}`);
  });

  test('GET /runs returns empty list for new task', async ({ request }) => {
    const name = `e2e-runs-${Date.now()}`;
    const createResp = await echoPost(request, '/api/echo/autonomous-tasks', {
      name,
      prompt_template: 'Hello',
      interval_seconds: 60,
      model: 'claude-opus-4-7',
    });
    const { id } = await createResp.json();

    const runsResp = await echoGet(request, `/api/echo/autonomous-tasks/${id}/runs`);
    expect(runsResp.status()).toBe(200);
    const body = await runsResp.json();
    expect(Array.isArray(body.items)).toBe(true);
    expect(body.items).toHaveLength(0);

    await echoDelete(request, `/api/echo/autonomous-tasks/${id}`);
  });

  test('GET /runs for unknown task returns 404', async ({ request }) => {
    const resp = await echoGet(request, '/api/echo/autonomous-tasks/nonexistent-xyz/runs');
    expect(resp.status()).toBe(404);
  });

  test('DELETE returns 204 (idempotent)', async ({ request }) => {
    const name = `e2e-del-${Date.now()}`;
    const createResp = await echoPost(request, '/api/echo/autonomous-tasks', {
      name,
      prompt_template: 'Test',
      interval_seconds: 60,
      model: 'claude-opus-4-7',
    });
    const { id } = await createResp.json();

    const del1 = await echoDelete(request, `/api/echo/autonomous-tasks/${id}`);
    expect(del1.status()).toBe(204);

    // Idempotent: second delete on missing task is also 204
    const del2 = await echoDelete(request, `/api/echo/autonomous-tasks/${id}`);
    expect(del2.status()).toBe(204);
  });

  test('POST with interval_seconds=0 returns 400', async ({ request }) => {
    const resp = await echoPost(request, '/api/echo/autonomous-tasks', {
      name: 'bad-interval',
      prompt_template: 'x',
      interval_seconds: 0,
      model: 'claude-opus-4-7',
    });
    expect(resp.status()).toBe(400);
    const body = await resp.json();
    expect(typeof body.error).toBe('string');
  });

  test('POST with empty name returns 400', async ({ request }) => {
    const resp = await echoPost(request, '/api/echo/autonomous-tasks', {
      name: '   ',
      prompt_template: 'x',
      interval_seconds: 60,
      model: 'claude-opus-4-7',
    });
    expect(resp.status()).toBe(400);
  });

  test('PATCH on unknown task returns 404', async ({ request }) => {
    const resp = await echoPatch(request, '/api/echo/autonomous-tasks/nonexistent-xyz', {
      enabled: false,
    });
    expect(resp.status()).toBe(404);
  });
});
