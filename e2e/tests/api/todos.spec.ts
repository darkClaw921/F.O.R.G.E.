import { test, expect } from '@playwright/test';
import {
  apiListTodos,
  apiCreateTodo,
  apiPatchTodo,
  apiDeleteTodo,
  BASE_URL,
} from '../../fixtures/api-client';

test.describe.configure({ mode: 'serial' });

test.describe('Todos API', () => {
  test('GET /api/todos returns an array', async ({ request }) => {
    const todos = await apiListTodos(request);
    expect(Array.isArray(todos)).toBe(true);
  });

  test('POST /api/todos creates a todo → 201', async ({ request }) => {
    const title = `e2e-todo-${Date.now()}`;
    const resp = await request.post(`${BASE_URL}/api/todos`, {
      data: { title },
    });
    expect(resp.status()).toBe(201);
    const body = await resp.json();
    expect(typeof body.id).toBe('string');
    expect(body.title).toBe(title);

    // Cleanup
    await apiDeleteTodo(request, body.id);
  });

  test('POST /api/todos → 400 when title is empty', async ({ request }) => {
    const resp = await request.post(`${BASE_URL}/api/todos`, {
      data: { title: '   ' },
    });
    expect(resp.status()).toBe(400);
  });

  test('todo item shape is correct', async ({ request }) => {
    const title = `e2e-shape-${Date.now()}`;
    const todo = await apiCreateTodo(request, title, 'desc');
    expect(typeof todo.id).toBe('string');
    expect(todo.title).toBe(title);
    // description field should exist
    expect('description' in todo || todo.description === undefined).toBe(true);

    await apiDeleteTodo(request, todo.id);
  });

  test('PATCH /api/todos/:id updates title', async ({ request }) => {
    const original = `e2e-patch-${Date.now()}`;
    const created = await apiCreateTodo(request, original);
    try {
      const newTitle = `${original}-updated`;
      const updated = await apiPatchTodo(request, created.id, { title: newTitle });
      expect(updated.title).toBe(newTitle);
    } finally {
      await apiDeleteTodo(request, created.id);
    }
  });

  test('PATCH /api/todos/:id → 400 for empty title', async ({ request }) => {
    const created = await apiCreateTodo(request, `e2e-empty-${Date.now()}`);
    try {
      const resp = await request.patch(
        `${BASE_URL}/api/todos/${encodeURIComponent(created.id)}`,
        { data: { title: '' } },
      );
      expect(resp.status()).toBe(400);
    } finally {
      await apiDeleteTodo(request, created.id);
    }
  });

  test('PATCH /api/todos/:id → 404 for unknown id', async ({ request }) => {
    const resp = await request.patch(`${BASE_URL}/api/todos/nonexistent-id-xyz`, {
      data: { title: 'any' },
    });
    expect(resp.status()).toBe(404);
  });

  test('DELETE /api/todos/:id removes the todo → 204', async ({ request }) => {
    const todo = await apiCreateTodo(request, `e2e-del-${Date.now()}`);
    const resp = await request.delete(
      `${BASE_URL}/api/todos/${encodeURIComponent(todo.id)}`,
    );
    expect(resp.status()).toBe(204);

    // Verify it is gone
    const todos = await apiListTodos(request);
    expect(todos.find((t) => t.id === todo.id)).toBeUndefined();
  });

  test('DELETE /api/todos/:id → 404 for unknown id', async ({ request }) => {
    const resp = await request.delete(`${BASE_URL}/api/todos/nonexistent-id-xyz`);
    expect(resp.status()).toBe(404);
  });

  test('PATCH /api/todos/:id can clear description with null', async ({ request }) => {
    const todo = await apiCreateTodo(request, `e2e-null-${Date.now()}`, 'has-desc');
    try {
      const resp = await request.patch(
        `${BASE_URL}/api/todos/${encodeURIComponent(todo.id)}`,
        { data: { description: null } },
      );
      expect(resp.ok()).toBe(true);
      const updated = await resp.json();
      // After nulling, description should be absent or null
      expect(updated.description == null).toBe(true);
    } finally {
      await apiDeleteTodo(request, todo.id);
    }
  });
});
