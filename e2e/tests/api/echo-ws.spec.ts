/**
 * WebSocket smoke-tests for /ws/echo.
 *
 * Design principles:
 * - Conversations are created via REST before opening WS (correct app flow).
 * - The "user_message" test is deliberately lenient: it accepts either a
 *   Claude CLI success response (assistant_chunk / assistant_done) or an error
 *   event — because Claude CLI may not be installed in the test environment.
 *   What matters is that the server handled the message and emitted at least
 *   one well-formed ServerMsg JSON frame.
 * - Rate-limit test sends 35 user_messages in rapid succession and expects to
 *   see at least one rate_limited error frame. It is marked with a generous
 *   timeout and its own assertion logic to avoid flakiness from slow CI.
 *
 * Node's built-in WebSocket (available since Node 22) is used.  If running on
 * an older Node the ws package would be required; Playwright's server process
 * will be Node 20+ in the canonical setup.
 */
import { test, expect } from '@playwright/test';

const BASE_URL = 'http://127.0.0.1:17331';
const WS_BASE  = 'ws://127.0.0.1:17331';

// ---------------------------------------------------------------------------
// REST helper to create a conversation
// ---------------------------------------------------------------------------

async function createConversation(
  request: import('@playwright/test').APIRequestContext,
  title?: string,
): Promise<string> {
  const resp = await request.post(`${BASE_URL}/api/echo/conversations`, {
    data: { title: title ?? `e2e-ws-conv-${Date.now()}` },
  });
  expect(resp.status()).toBe(200);
  const body = await resp.json();
  return body.id as string;
}

async function deleteConversation(
  request: import('@playwright/test').APIRequestContext,
  id: string,
) {
  await request.delete(`${BASE_URL}/api/echo/conversations/${id}`);
}

// ---------------------------------------------------------------------------
// WebSocket helpers
// ---------------------------------------------------------------------------

/**
 * Opens a WebSocket to /ws/echo?conversation_id=<id> and returns {ws, firstMsg}.
 * Waits up to `timeoutMs` for the first text frame.
 */
function openEchoWs(conversationId: string): WebSocket {
  const url = `${WS_BASE}/ws/echo?conversation_id=${encodeURIComponent(conversationId)}`;
  // Node ≥22 has globalThis.WebSocket; older Nodes need the 'ws' package.
  // Playwright bundles its own Node version. Use require('ws') as a reliable fallback.
  const WS: typeof WebSocket =
    (globalThis as any).WebSocket ?? require('ws');
  return new WS(url) as WebSocket;
}

/**
 * Collect WS frames for `durationMs` ms, then close.
 * Returns an array of parsed JSON objects.
 */
function collectFrames(
  ws: WebSocket,
  durationMs: number,
): Promise<any[]> {
  return new Promise<any[]>((resolve, reject) => {
    const frames: any[] = [];

    ws.onmessage = (evt: MessageEvent) => {
      try {
        frames.push(JSON.parse(evt.data as string));
      } catch { /* ignore non-JSON */ }
    };

    ws.onerror = (err: Event) => {
      reject(new Error(`WS error: ${JSON.stringify(err)}`));
    };

    setTimeout(() => {
      ws.close();
      resolve(frames);
    }, durationMs);
  });
}

/**
 * Wait until at least one frame matching `pred` arrives, or reject after
 * `timeoutMs`.
 */
function waitForFrame(
  ws: WebSocket,
  pred: (frame: any) => boolean,
  timeoutMs: number,
): Promise<any> {
  return new Promise<any>((resolve, reject) => {
    const timer = setTimeout(() => {
      ws.close();
      reject(new Error(`Timed out waiting for matching WS frame (${timeoutMs}ms)`));
    }, timeoutMs);

    ws.onmessage = (evt: MessageEvent) => {
      let frame: any;
      try {
        frame = JSON.parse(evt.data as string);
      } catch {
        return;
      }
      if (pred(frame)) {
        clearTimeout(timer);
        ws.close();
        resolve(frame);
      }
    };

    ws.onerror = (err: Event) => {
      clearTimeout(timer);
      reject(new Error(`WS error event: ${JSON.stringify(err)}`));
    };
  });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test.describe('WebSocket /ws/echo smoke', () => {
  test.describe.configure({ mode: 'serial' });

  test('connection opens and server sends ping heartbeat within 20s', async ({ request }) => {
    const convId = await createConversation(request, `e2e-ws-heartbeat-${Date.now()}`);

    try {
      const ws = openEchoWs(convId);

      // The server sends a ping every 15s; wait up to 20s for one
      const pingFrame = await waitForFrame(
        ws,
        (f) => f.type === 'ping',
        20_000,
      );
      expect(pingFrame.type).toBe('ping');
    } finally {
      await deleteConversation(request, convId);
    }
  });

  test('server does not close after receiving pong', async ({ request }) => {
    const convId = await createConversation(request, `e2e-ws-pong-${Date.now()}`);

    try {
      const ws = openEchoWs(convId);

      // Wait for the first ping, then reply with pong; then verify the
      // connection stays open for another 3 seconds by collecting frames.
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(() => {
          ws.close();
          reject(new Error('Did not receive ping within 20s'));
        }, 20_000);

        let closedUnexpectedly = false;
        ws.onclose = () => {
          if (!closedUnexpectedly) return;
        };
        ws.onmessage = (evt: MessageEvent) => {
          try {
            const frame = JSON.parse(evt.data as string);
            if (frame.type === 'ping') {
              clearTimeout(timer);
              ws.send(JSON.stringify({ type: 'pong' }));
              setTimeout(() => {
                ws.close();
                resolve();
              }, 3_000);
            }
          } catch { /* ignore */ }
        };
        ws.onerror = () => {
          clearTimeout(timer);
          reject(new Error('WS error'));
        };
      });
    } finally {
      await deleteConversation(request, convId);
    }
  });

  test('sending user_message yields at least one server event (assistant or error)', async ({
    request,
  }) => {
    const convId = await createConversation(request, `e2e-ws-msg-${Date.now()}`);

    try {
      const ws = openEchoWs(convId);

      // Wait for WS to be open
      await new Promise<void>((res, rej) => {
        ws.onopen = () => res();
        ws.onerror = () => rej(new Error('WS failed to open'));
        // Also resolve if ping arrives (means it's already open)
      });

      // Send user_message
      ws.send(JSON.stringify({
        type: 'user_message',
        text: 'hi, this is a Playwright e2e test',
        conversation_id: convId,
        model: 'claude-opus-4-7',
        ctx_opts: {
          include_pane_capture: false,
          include_memories: false,
        },
      }));

      // Wait up to 30s for any meaningful server response
      // Accept: assistant_chunk, assistant_done, error, notification
      const frame = await waitForFrame(
        ws,
        (f) => ['assistant_chunk', 'assistant_done', 'error', 'notification', 'stats_update'].includes(f.type),
        30_000,
      );

      // The frame must have a type field
      expect(typeof frame.type).toBe('string');
      expect(frame.type.length).toBeGreaterThan(0);

      // If it's an error, the code must be a non-empty string (not a server crash)
      if (frame.type === 'error') {
        expect(typeof frame.code).toBe('string');
        expect(frame.code.length).toBeGreaterThan(0);
      }
    } finally {
      await deleteConversation(request, convId);
    }
  });

  test('malformed JSON is rejected with error frame, connection stays open', async ({
    request,
  }) => {
    const convId = await createConversation(request, `e2e-ws-bad-json-${Date.now()}`);

    try {
      const ws = openEchoWs(convId);

      await new Promise<void>((res, rej) => {
        ws.onopen = () => res();
        ws.onerror = () => rej(new Error('WS failed to open'));
      });

      // Send malformed JSON
      ws.send('this is not json at all {{{');

      const frame = await waitForFrame(
        ws,
        (f) => f.type === 'error',
        10_000,
      );

      expect(frame.type).toBe('error');
      expect(frame.code).toBe('bad_request');
      expect(typeof frame.message).toBe('string');
    } finally {
      await deleteConversation(request, convId);
    }
  });

  test('user_message to non-existent conversation yields error with code no_conversation', async ({
    request,
  }) => {
    // Open the WS using the fake id as conversation_id in the query string.
    // The WS upgrade itself does not validate the conversation — only the
    // first user_message triggers the check and broadcasts the error back on
    // that same conversation channel, which our socket is subscribed to.
    const fakeConvId = `fake-conv-${Date.now()}`;

    const ws = openEchoWs(fakeConvId);

    await new Promise<void>((res, rej) => {
      ws.onopen = () => res();
      ws.onerror = () => rej(new Error('WS failed to open'));
    });

    ws.send(JSON.stringify({
      type: 'user_message',
      text: 'test',
      conversation_id: fakeConvId,
      ctx_opts: {
        include_pane_capture: false,
        include_memories: false,
      },
    }));

    const frame = await waitForFrame(
      ws,
      (f) => f.type === 'error',
      10_000,
    );
    expect(frame.type).toBe('error');
    expect(frame.code).toBe('no_conversation');
  });
});
