import { test, expect } from '@playwright/test';
import {
  apiListThemes,
  apiGetActiveTheme,
  apiPatchActiveTheme,
  apiCreateCustomTheme,
  apiDeleteCustomTheme,
  minimalTheme,
  BASE_URL,
} from '../../fixtures/api-client';

test.describe.configure({ mode: 'serial' });

let customThemeId: string | null = null;
let originalActiveId: string | null = null;

test.beforeAll(async ({ request }) => {
  const active = await apiGetActiveTheme(request);
  originalActiveId = active.id;
});

test.afterAll(async ({ request }) => {
  // Restore original active theme
  if (originalActiveId) {
    try {
      await apiPatchActiveTheme(request, originalActiveId);
    } catch { /* best-effort */ }
  }
  // Delete any leftover custom theme
  if (customThemeId) {
    try {
      // Switch away from custom theme first if it's still active
      const active = await apiGetActiveTheme(request);
      if (active.id === customThemeId && originalActiveId) {
        await apiPatchActiveTheme(request, originalActiveId);
      }
      await apiDeleteCustomTheme(request, customThemeId);
    } catch { /* best-effort */ }
    customThemeId = null;
  }
});

test.describe('Themes API', () => {
  test('GET /api/themes returns presets, custom, and active', async ({ request }) => {
    const body = await apiListThemes(request);
    expect(Array.isArray(body.presets)).toBe(true);
    expect(body.presets.length).toBeGreaterThan(0);
    expect(Array.isArray(body.custom)).toBe(true);
    expect(typeof body.active).toBe('string');
  });

  test('GET /api/themes presets include "default"', async ({ request }) => {
    const body = await apiListThemes(request);
    const ids = body.presets.map((p: any) => p.id);
    expect(ids).toContain('default');
  });

  test('GET /api/themes/active returns a valid Theme object', async ({ request }) => {
    const theme = await apiGetActiveTheme(request);
    expect(typeof theme.id).toBe('string');
    expect(typeof theme.name).toBe('string');
    expect(theme.ui).toBeDefined();
    expect(theme.term).toBeDefined();
    // Check some colour fields exist
    expect(typeof theme.ui.bg).toBe('string');
    expect(typeof theme.term.foreground).toBe('string');
  });

  test('PATCH /api/themes/active switches to a preset', async ({ request }) => {
    const body = await apiListThemes(request);
    // Pick a preset that is NOT currently active
    const otherPreset = body.presets.find((p: any) => p.id !== body.active);
    if (!otherPreset) {
      // Only one preset somehow, skip
      return;
    }
    const result = await apiPatchActiveTheme(request, otherPreset.id);
    expect(result.active).toBe(otherPreset.id);

    // Verify via GET
    const active = await apiGetActiveTheme(request);
    expect(active.id).toBe(otherPreset.id);
  });

  test('PATCH /api/themes/active → 404 for unknown theme id', async ({ request }) => {
    const resp = await request.patch(`${BASE_URL}/api/themes/active`, {
      data: { id: 'theme-that-does-not-exist' },
    });
    expect(resp.status()).toBe(404);
  });

  test('PATCH /api/themes/active → 400 for empty id', async ({ request }) => {
    const resp = await request.patch(`${BASE_URL}/api/themes/active`, {
      data: { id: '' },
    });
    expect(resp.status()).toBe(400);
  });

  test('POST /api/themes/custom creates a custom theme → 201', async ({ request }) => {
    const theme = minimalTheme({ name: `e2e-custom-${Date.now()}` });
    const resp = await request.post(`${BASE_URL}/api/themes/custom`, { data: theme });
    expect(resp.status()).toBe(201);
    const created = await resp.json();
    expect(typeof created.id).toBe('string');
    customThemeId = created.id;

    // Should appear in themes list
    const list = await apiListThemes(request);
    const found = list.custom.find((c: any) => c.id === customThemeId);
    expect(found).toBeDefined();
  });

  test('POST /api/themes/custom → 409 when id conflicts with preset', async ({ request }) => {
    const theme = minimalTheme({ id: 'default' });
    const resp = await request.post(`${BASE_URL}/api/themes/custom`, { data: theme });
    expect(resp.status()).toBe(409);
  });

  test('PUT /api/themes/custom/:id replaces the custom theme', async ({ request }) => {
    if (!customThemeId) { test.skip(); return; }

    const updated = minimalTheme({ id: customThemeId, name: 'updated-name' });
    const resp = await request.put(
      `${BASE_URL}/api/themes/custom/${encodeURIComponent(customThemeId)}`,
      { data: updated },
    );
    expect(resp.ok()).toBe(true);
    const body = await resp.json();
    expect(body.name).toBe('updated-name');
  });

  test('PUT /api/themes/custom/:id → 404 for unknown id', async ({ request }) => {
    const resp = await request.put(`${BASE_URL}/api/themes/custom/nonexistent-id`, {
      data: minimalTheme(),
    });
    expect(resp.status()).toBe(404);
  });

  test('DELETE /api/themes/custom/:id removes the custom theme → 204', async ({ request }) => {
    if (!customThemeId) { test.skip(); return; }

    // Ensure it is not the active theme
    const body = await apiListThemes(request);
    if (body.active === customThemeId && originalActiveId) {
      await apiPatchActiveTheme(request, originalActiveId);
    }

    const resp = await request.delete(
      `${BASE_URL}/api/themes/custom/${encodeURIComponent(customThemeId)}`,
    );
    expect(resp.status()).toBe(204);

    const list = await apiListThemes(request);
    expect(list.custom.find((c: any) => c.id === customThemeId)).toBeUndefined();
    customThemeId = null;
  });

  test('DELETE /api/themes/custom/:id → 409 when theme is active', async ({ request }) => {
    // Create a custom theme, set it active, then try to delete
    const theme = minimalTheme({ name: `e2e-active-del-${Date.now()}` });
    const created = await apiCreateCustomTheme(request, theme);
    const id = created.id as string;
    await apiPatchActiveTheme(request, id);
    try {
      const resp = await request.delete(
        `${BASE_URL}/api/themes/custom/${encodeURIComponent(id)}`,
      );
      expect(resp.status()).toBe(409);
    } finally {
      // Restore and delete
      if (originalActiveId) await apiPatchActiveTheme(request, originalActiveId);
      await apiDeleteCustomTheme(request, id);
    }
  });

  test('DELETE /api/themes/custom/:id → 404 for unknown id', async ({ request }) => {
    const resp = await request.delete(`${BASE_URL}/api/themes/custom/nonexistent-id`);
    expect(resp.status()).toBe(404);
  });
});
