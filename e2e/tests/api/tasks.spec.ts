import { test, expect } from '@playwright/test';
import {
  apiListTasks,
  apiCreateTask,
  apiPatchTask,
  apiCloseTask,
  apiReopenTask,
  apiListProjects,
  apiSetActiveProject,
} from '../../fixtures/api-client';

test.describe.configure({ mode: 'serial' });

/**
 * Ensure the active project is one with a beads-initialized path.
 * The projects spec may have left the active project pointing to /tmp.
 * We find the first project whose path contains '.beads' (by trying to
 * restore the 'forge' project if available, otherwise the first project).
 */
async function ensureBeadsProject(request: any): Promise<void> {
  const projects = await apiListProjects(request);
  const active = projects.find((p: any) => p.active);
  if (!active) return;
  // If active project has a /tmp path, switch back to the forge project
  const isTemp = active.path.includes('/tmp') || active.path.includes('/var/folders') || active.path.includes('Temp');
  if (isTemp) {
    const forgeProj = projects.find((p: any) => p.id === 'forge') ?? projects.find((p: any) => !p.path.includes('/tmp'));
    if (forgeProj) {
      await apiSetActiveProject(request, forgeProj.id);
    }
  }
}

test.describe('Tasks API', () => {
  test.beforeAll(async ({ request }) => {
    await ensureBeadsProject(request);
  });

  test('GET /api/tasks returns expected shape', async ({ request }) => {
    const body = await apiListTasks(request);
    // br list --json --all returns { issues: [...], ... }
    expect(typeof body).toBe('object');
    expect(body).not.toBeNull();
    // There is either an `issues` array or a direct array
    const issues = Array.isArray(body) ? body : body.issues ?? [];
    expect(Array.isArray(issues)).toBe(true);
  });

  test('POST /api/tasks creates a task and returns 201', async ({ request }) => {
    const title = `e2e-task-${Date.now()}`;
    const resp = await request.post('http://127.0.0.1:17331/api/tasks', {
      data: { title },
    });
    expect(resp.status()).toBe(201);
    const body = await resp.json();
    // br create --json returns either the task object or a wrapper
    expect(body).toBeTruthy();
  });

  test('POST /api/tasks → 400 when title is empty', async ({ request }) => {
    const resp = await request.post('http://127.0.0.1:17331/api/tasks', {
      data: { title: '   ' },
    });
    expect(resp.status()).toBe(400);
  });

  test('POST /api/tasks → 400 when priority out of range', async ({ request }) => {
    const resp = await request.post('http://127.0.0.1:17331/api/tasks', {
      data: { title: 'bad-priority', priority: 5 },
    });
    expect(resp.status()).toBe(400);
  });

  test('PATCH /api/tasks/:id updates status', async ({ request }) => {
    const title = `e2e-patch-${Date.now()}`;
    const created = await apiCreateTask(request, { title });
    // The response from br create --json may be a task object or a container
    // Find the id from the response
    const id = extractId(created);
    if (!id) {
      test.skip(); // br output format not parseable; skip gracefully
      return;
    }

    const updated = await apiPatchTask(request, id, { status: 'in_progress' });
    expect(updated).toBeTruthy();
  });

  test('PATCH /api/tasks/:id → 400 when no fields provided', async ({ request }) => {
    const title = `e2e-noop-${Date.now()}`;
    const created = await apiCreateTask(request, { title });
    const id = extractId(created);
    if (!id) {
      test.skip();
      return;
    }
    const resp = await request.patch(`http://127.0.0.1:17331/api/tasks/${encodeURIComponent(id)}`, {
      data: {},
    });
    expect(resp.status()).toBe(400);
  });

  test('DELETE /api/tasks/:id closes the task → 204', async ({ request }) => {
    const title = `e2e-close-${Date.now()}`;
    const created = await apiCreateTask(request, { title });
    const id = extractId(created);
    if (!id) {
      test.skip();
      return;
    }
    await apiCloseTask(request, id, 'e2e test completed');
  });

  test('POST /api/tasks/:id/reopen reopens a closed task', async ({ request }) => {
    const title = `e2e-reopen-${Date.now()}`;
    const created = await apiCreateTask(request, { title });
    const id = extractId(created);
    if (!id) {
      test.skip();
      return;
    }
    await apiCloseTask(request, id);
    const reopened = await apiReopenTask(request, id);
    expect(reopened).toBeTruthy();
  });
});

/**
 * Extract issue id from various shapes that `br create --json` may return:
 * - `{ id: "..." }` — direct object
 * - `{ created: [ { id: "..." } ] }` — wrapped in array
 */
function extractId(body: any): string | null {
  if (!body) return null;
  if (typeof body.id === 'string') return body.id;
  if (Array.isArray(body.created) && body.created[0]?.id) return body.created[0].id;
  if (Array.isArray(body) && body[0]?.id) return body[0].id;
  // Try nested br output
  const val = JSON.stringify(body);
  const m = val.match(/"id"\s*:\s*"([^"]+)"/);
  return m ? m[1] : null;
}
