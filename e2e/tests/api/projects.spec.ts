import { test, expect } from '@playwright/test';
import os from 'os';
import {
  apiListProjects,
  apiCreateProject,
  apiDeleteProject,
  apiSetActiveProject,
  BASE_URL,
} from '../../fixtures/api-client';

test.describe.configure({ mode: 'serial' });

let createdProjectId: string | null = null;
let originalActiveId: string | null = null;

test.beforeAll(async ({ request }) => {
  // Record the original active project ID so we can restore it in afterAll
  const projects = await apiListProjects(request);
  const active = projects.find((p) => p.active);
  originalActiveId = active?.id ?? null;
});

test.afterAll(async ({ request }) => {
  // Restore the original active project
  if (originalActiveId) {
    try {
      await apiSetActiveProject(request, originalActiveId);
    } catch { /* best-effort */ }
  }
  // Delete the test project if it still exists
  if (createdProjectId) {
    try {
      await apiDeleteProject(request, createdProjectId);
    } catch { /* best-effort */ }
    createdProjectId = null;
  }
});

test.describe('Projects API', () => {
  test('GET /api/projects returns array with at least one project', async ({ request }) => {
    const projects = await apiListProjects(request);
    expect(Array.isArray(projects)).toBe(true);
    expect(projects.length).toBeGreaterThanOrEqual(1);
  });

  test('GET /api/projects items have expected shape', async ({ request }) => {
    const projects = await apiListProjects(request);
    const p = projects[0];
    expect(typeof p.id).toBe('string');
    expect(typeof p.name).toBe('string');
    expect(typeof p.path).toBe('string');
    expect(typeof p.active).toBe('boolean');
    expect(typeof p.tmux_prefix).toBe('string');
    expect(typeof p.origin).toBe('string');
  });

  test('exactly one project is active', async ({ request }) => {
    const projects = await apiListProjects(request);
    const activeProjects = projects.filter((p) => p.active);
    expect(activeProjects.length).toBe(1);
  });

  test('POST /api/projects creates a project → 201', async ({ request }) => {
    const name = `e2e-proj-${Date.now()}`;
    const projPath = os.tmpdir();
    const resp = await request.post(`${BASE_URL}/api/projects`, {
      data: { name, path: projPath },
    });
    expect(resp.status()).toBe(201);
    const body = await resp.json();
    expect(typeof body.id).toBe('string');
    createdProjectId = body.id;
  });

  test('POST /api/projects → 400 when name is missing', async ({ request }) => {
    const resp = await request.post(`${BASE_URL}/api/projects`, {
      data: { path: os.tmpdir() },
    });
    // Missing name field → 400 or 422
    expect(resp.status()).toBeGreaterThanOrEqual(400);
    expect(resp.status()).toBeLessThan(500);
  });

  test('DELETE /api/projects/:id removes non-active project', async ({ request }) => {
    // Create a project, ensure it is not active, then delete it
    const name = `e2e-del-${Date.now()}`;
    const created = await apiCreateProject(request, name, os.tmpdir());
    const id = created.id as string;

    // Restore original active before deleting if needed
    if (originalActiveId) {
      try { await apiSetActiveProject(request, originalActiveId); } catch { /* ignore */ }
    }

    await apiDeleteProject(request, id);

    const after = await apiListProjects(request);
    expect(after.find((p) => p.id === id)).toBeUndefined();
  });

  test('DELETE /api/projects/:id → 409 when trying to delete active project', async ({
    request,
  }) => {
    const projects = await apiListProjects(request);
    const active = projects.find((p) => p.active)!;
    const resp = await request.delete(`${BASE_URL}/api/projects/${encodeURIComponent(active.id)}`);
    expect(resp.status()).toBe(409);
  });

  test('DELETE /api/projects/:id → 404 for unknown id', async ({ request }) => {
    const resp = await request.delete(`${BASE_URL}/api/projects/nonexistent-id-xyz`);
    expect(resp.status()).toBe(404);
  });

  test('POST /api/projects/active switches active project', async ({ request }) => {
    if (!createdProjectId) {
      test.skip();
      return;
    }
    await apiSetActiveProject(request, createdProjectId);
    const projects = await apiListProjects(request);
    const active = projects.find((p) => p.active)!;
    expect(active.id).toBe(createdProjectId);
  });

  test('PATCH /api/projects/:id/settings updates settings', async ({ request }) => {
    if (!createdProjectId) {
      test.skip();
      return;
    }
    const resp = await request.patch(
      `${BASE_URL}/api/projects/${encodeURIComponent(createdProjectId)}/settings`,
      { data: { notify_delay_minutes: 5 } },
    );
    expect(resp.ok()).toBe(true);
  });
});
