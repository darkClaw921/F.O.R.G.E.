import { test, expect } from '@playwright/test';
import { apiHealthz } from '../../fixtures/api-client';

test.describe('GET /healthz', () => {
  test('returns status ok and expected shape', async ({ request }) => {
    const body = await apiHealthz(request);

    expect(body.status).toBe('ok');
    expect(typeof body.remote_mode).toBe('boolean');
    expect(typeof body.version).toBe('string');
    expect(body.version.length).toBeGreaterThan(0);
  });

  test('remote_mode is false in default local mode', async ({ request }) => {
    const body = await apiHealthz(request);
    // Default server started without --remote flag
    expect(body.remote_mode).toBe(false);
  });

  test('responds without auth header (publicly accessible)', async ({ request }) => {
    const resp = await request.get('http://127.0.0.1:17331/healthz');
    expect(resp.ok()).toBe(true);
    expect(resp.status()).toBe(200);
  });
});
