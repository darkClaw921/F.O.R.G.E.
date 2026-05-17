/**
 * Thin REST wrapper for F.O.R.G.E. / devforge API.
 *
 * Uses Playwright's `APIRequestContext` so that the caller does not need
 * to manage fetch/node-fetch separately. Every helper throws on non-2xx
 * unless the caller explicitly handles the response.
 */
import type { APIRequestContext } from '@playwright/test';

export const BASE_URL = `http://127.0.0.1:17331`;

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export async function apiListSessions(request: APIRequestContext): Promise<any[]> {
  const resp = await request.get(`${BASE_URL}/api/sessions`);
  if (!resp.ok()) throw new Error(`GET /api/sessions → ${resp.status()}`);
  return resp.json();
}

export async function apiCreateSession(request: APIRequestContext, name: string): Promise<void> {
  const resp = await request.post(`${BASE_URL}/api/sessions`, { data: { name } });
  if (resp.status() !== 201) {
    const body = await resp.text();
    throw new Error(`POST /api/sessions → ${resp.status()}: ${body}`);
  }
}

export async function apiDeleteSession(request: APIRequestContext, name: string): Promise<void> {
  const resp = await request.delete(`${BASE_URL}/api/sessions/${encodeURIComponent(name)}`);
  if (resp.status() !== 204) {
    const body = await resp.text();
    throw new Error(`DELETE /api/sessions/${name} → ${resp.status()}: ${body}`);
  }
}

export async function apiRenameSession(
  request: APIRequestContext,
  oldName: string,
  newName: string,
): Promise<{ name: string }> {
  const resp = await request.patch(`${BASE_URL}/api/sessions/${encodeURIComponent(oldName)}`, {
    data: { name: newName },
  });
  if (!resp.ok()) {
    const body = await resp.text();
    throw new Error(`PATCH /api/sessions/${oldName} → ${resp.status()}: ${body}`);
  }
  return resp.json();
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

export async function apiListWindows(request: APIRequestContext, session: string): Promise<any[]> {
  const resp = await request.get(`${BASE_URL}/api/sessions/${encodeURIComponent(session)}/windows`);
  if (!resp.ok()) throw new Error(`GET /api/sessions/${session}/windows → ${resp.status()}`);
  return resp.json();
}

export async function apiCreateWindow(
  request: APIRequestContext,
  session: string,
  name?: string,
): Promise<void> {
  const resp = await request.post(
    `${BASE_URL}/api/sessions/${encodeURIComponent(session)}/windows`,
    { data: name ? { name } : {} },
  );
  if (resp.status() !== 201) {
    const body = await resp.text();
    throw new Error(`POST /api/sessions/${session}/windows → ${resp.status()}: ${body}`);
  }
}

export async function apiSelectWindow(
  request: APIRequestContext,
  session: string,
  index: number,
): Promise<void> {
  const resp = await request.post(
    `${BASE_URL}/api/sessions/${encodeURIComponent(session)}/windows/${index}/select`,
  );
  if (resp.status() !== 204) {
    const body = await resp.text();
    throw new Error(`POST .../windows/${index}/select → ${resp.status()}: ${body}`);
  }
}

export async function apiDeleteWindow(
  request: APIRequestContext,
  session: string,
  index: number,
): Promise<void> {
  const resp = await request.delete(
    `${BASE_URL}/api/sessions/${encodeURIComponent(session)}/windows/${index}`,
  );
  if (resp.status() !== 204) {
    const body = await resp.text();
    throw new Error(`DELETE .../windows/${index} → ${resp.status()}: ${body}`);
  }
}

export async function apiRenameWindow(
  request: APIRequestContext,
  session: string,
  index: number,
  newName: string,
): Promise<{ name: string }> {
  const resp = await request.patch(
    `${BASE_URL}/api/sessions/${encodeURIComponent(session)}/windows/${index}`,
    { data: { name: newName } },
  );
  if (!resp.ok()) {
    const body = await resp.text();
    throw new Error(`PATCH .../windows/${index} → ${resp.status()}: ${body}`);
  }
  return resp.json();
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

export async function apiListTasks(request: APIRequestContext): Promise<any> {
  const resp = await request.get(`${BASE_URL}/api/tasks`);
  if (!resp.ok()) throw new Error(`GET /api/tasks → ${resp.status()}`);
  return resp.json();
}

export interface CreateTaskOpts {
  title: string;
  issue_type?: string;
  priority?: number;
  description?: string;
}

export async function apiCreateTask(
  request: APIRequestContext,
  opts: CreateTaskOpts,
): Promise<any> {
  const resp = await request.post(`${BASE_URL}/api/tasks`, { data: opts });
  if (resp.status() !== 201) {
    const body = await resp.text();
    throw new Error(`POST /api/tasks → ${resp.status()}: ${body}`);
  }
  return resp.json();
}

export async function apiPatchTask(
  request: APIRequestContext,
  id: string,
  fields: Record<string, unknown>,
): Promise<any> {
  const resp = await request.patch(`${BASE_URL}/api/tasks/${encodeURIComponent(id)}`, {
    data: fields,
  });
  if (!resp.ok()) {
    const body = await resp.text();
    throw new Error(`PATCH /api/tasks/${id} → ${resp.status()}: ${body}`);
  }
  return resp.json();
}

export async function apiCloseTask(request: APIRequestContext, id: string, reason = ''): Promise<void> {
  const url = `${BASE_URL}/api/tasks/${encodeURIComponent(id)}${reason ? `?reason=${encodeURIComponent(reason)}` : ''}`;
  const resp = await request.delete(url);
  if (resp.status() !== 204) {
    const body = await resp.text();
    throw new Error(`DELETE /api/tasks/${id} → ${resp.status()}: ${body}`);
  }
}

export async function apiReopenTask(request: APIRequestContext, id: string): Promise<any> {
  const resp = await request.post(`${BASE_URL}/api/tasks/${encodeURIComponent(id)}/reopen`);
  if (!resp.ok()) {
    const body = await resp.text();
    throw new Error(`POST /api/tasks/${id}/reopen → ${resp.status()}: ${body}`);
  }
  return resp.json();
}

// ---------------------------------------------------------------------------
// Todos
// ---------------------------------------------------------------------------

export async function apiListTodos(request: APIRequestContext): Promise<any[]> {
  const resp = await request.get(`${BASE_URL}/api/todos`);
  if (!resp.ok()) throw new Error(`GET /api/todos → ${resp.status()}`);
  return resp.json();
}

export async function apiCreateTodo(
  request: APIRequestContext,
  title: string,
  description?: string,
): Promise<any> {
  const resp = await request.post(`${BASE_URL}/api/todos`, {
    data: { title, description },
  });
  if (resp.status() !== 201) {
    const body = await resp.text();
    throw new Error(`POST /api/todos → ${resp.status()}: ${body}`);
  }
  return resp.json();
}

export async function apiPatchTodo(
  request: APIRequestContext,
  id: string,
  fields: Record<string, unknown>,
): Promise<any> {
  const resp = await request.patch(`${BASE_URL}/api/todos/${encodeURIComponent(id)}`, {
    data: fields,
  });
  if (!resp.ok()) {
    const body = await resp.text();
    throw new Error(`PATCH /api/todos/${id} → ${resp.status()}: ${body}`);
  }
  return resp.json();
}

export async function apiDeleteTodo(request: APIRequestContext, id: string): Promise<void> {
  const resp = await request.delete(`${BASE_URL}/api/todos/${encodeURIComponent(id)}`);
  if (resp.status() !== 204) {
    const body = await resp.text();
    throw new Error(`DELETE /api/todos/${id} → ${resp.status()}: ${body}`);
  }
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

export async function apiListProjects(request: APIRequestContext): Promise<any[]> {
  const resp = await request.get(`${BASE_URL}/api/projects`);
  if (!resp.ok()) throw new Error(`GET /api/projects → ${resp.status()}`);
  return resp.json();
}

export async function apiCreateProject(
  request: APIRequestContext,
  name: string,
  projPath: string,
  tmuxPrefix?: string,
): Promise<any> {
  const resp = await request.post(`${BASE_URL}/api/projects`, {
    data: { name, path: projPath, tmux_prefix: tmuxPrefix },
  });
  if (resp.status() !== 201) {
    const body = await resp.text();
    throw new Error(`POST /api/projects → ${resp.status()}: ${body}`);
  }
  return resp.json();
}

export async function apiDeleteProject(request: APIRequestContext, id: string): Promise<void> {
  const resp = await request.delete(`${BASE_URL}/api/projects/${encodeURIComponent(id)}`);
  if (resp.status() !== 204) {
    const body = await resp.text();
    throw new Error(`DELETE /api/projects/${id} → ${resp.status()}: ${body}`);
  }
}

export async function apiSetActiveProject(request: APIRequestContext, id: string): Promise<void> {
  const resp = await request.post(`${BASE_URL}/api/projects/active`, { data: { id } });
  if (!resp.ok()) {
    const body = await resp.text();
    throw new Error(`POST /api/projects/active → ${resp.status()}: ${body}`);
  }
}

// ---------------------------------------------------------------------------
// Themes
// ---------------------------------------------------------------------------

export async function apiListThemes(request: APIRequestContext): Promise<any> {
  const resp = await request.get(`${BASE_URL}/api/themes`);
  if (!resp.ok()) throw new Error(`GET /api/themes → ${resp.status()}`);
  return resp.json();
}

export async function apiGetActiveTheme(request: APIRequestContext): Promise<any> {
  const resp = await request.get(`${BASE_URL}/api/themes/active`);
  if (!resp.ok()) throw new Error(`GET /api/themes/active → ${resp.status()}`);
  return resp.json();
}

export async function apiPatchActiveTheme(request: APIRequestContext, id: string): Promise<any> {
  const resp = await request.patch(`${BASE_URL}/api/themes/active`, { data: { id } });
  if (!resp.ok()) {
    const body = await resp.text();
    throw new Error(`PATCH /api/themes/active → ${resp.status()}: ${body}`);
  }
  return resp.json();
}

export async function apiCreateCustomTheme(request: APIRequestContext, theme: unknown): Promise<any> {
  const resp = await request.post(`${BASE_URL}/api/themes/custom`, { data: theme });
  if (resp.status() !== 201) {
    const body = await resp.text();
    throw new Error(`POST /api/themes/custom → ${resp.status()}: ${body}`);
  }
  return resp.json();
}

export async function apiDeleteCustomTheme(request: APIRequestContext, id: string): Promise<void> {
  const resp = await request.delete(`${BASE_URL}/api/themes/custom/${encodeURIComponent(id)}`);
  if (resp.status() !== 204) {
    const body = await resp.text();
    throw new Error(`DELETE /api/themes/custom/${id} → ${resp.status()}: ${body}`);
  }
}

// ---------------------------------------------------------------------------
// Healthz
// ---------------------------------------------------------------------------

export async function apiHealthz(request: APIRequestContext): Promise<any> {
  const resp = await request.get(`${BASE_URL}/healthz`);
  if (!resp.ok()) throw new Error(`GET /healthz → ${resp.status()}`);
  return resp.json();
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Generate a unique prefix for E2E test tmux session names. */
export function e2ePrefix(): string {
  return `e2e_${Date.now()}_`;
}

/**
 * Get the active project's tmux_prefix from the API.
 * The server auto-prepends `<prefix>-` to session names on creation.
 */
export async function getActiveProjectPrefix(request: APIRequestContext): Promise<string> {
  const resp = await request.get(`${BASE_URL}/api/projects`);
  if (!resp.ok()) return '';
  const projects: any[] = await resp.json();
  const active = projects.find((p) => p.active);
  return active?.tmux_prefix ?? '';
}

/**
 * Build the expected full session name as the server will store it.
 * If the prefix is non-empty: `<prefix>-<name>` else just `<name>`.
 */
export function fullSessionName(prefix: string, name: string): string {
  if (!prefix) return name;
  // Server calls ensure_prefixed: only prepends if not already prefixed
  const expected = `${prefix}-${name}`;
  if (name.startsWith(`${prefix}-`)) return name;
  return expected;
}

/**
 * A sample minimal custom theme payload for tests.
 * All colour fields are required by the Theme struct.
 */
export function minimalTheme(overrides: Partial<{ id: string; name: string }> = {}): unknown {
  const base = '#000000';
  return {
    id: overrides.id ?? '',
    name: overrides.name ?? `e2e-theme-${Date.now()}`,
    ui: {
      bg: base, bgElev: base, fg: base, fgDim: base, border: base,
      accent: base, warn: base, danger: base, p0: base, p1: base, p2: base,
    },
    term: {
      foreground: base, background: base, cursor: base, selection: base,
      black: base, red: base, green: base, yellow: base, blue: base,
      magenta: base, cyan: base, white: base, brightBlack: base, brightRed: base,
      brightGreen: base, brightYellow: base, brightBlue: base, brightMagenta: base,
      brightCyan: base, brightWhite: base,
    },
  };
}
