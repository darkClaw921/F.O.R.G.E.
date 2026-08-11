/**
 * UI tests for передачу файлов в терминальную сессию (forge-660j, forge-mgad).
 *
 * Покрывают оба жеста фронтенд-модуля static/js/terminal/paste-files.js:
 * - drag&drop файла в #terminal;
 * - вставка файла из буфера (Ctrl/Cmd+V).
 *
 * Оба события синтетические (Playwright не умеет тащить файл из ОС и класть
 * файл в системный буфер), но собираются из НАСТОЯЩИХ DataTransfer/File, т.е.
 * проходят ровно тот же путь, что и жест пользователя: обработчик → POST
 * /api/sessions/:name/files → тост с абсолютным путём → term.paste.
 *
 * Проверяется цепочка целиком: успех виден в тосте `.paste-toast`, где лежит
 * путь вида <root>/.forge/pasted/<имя>, а сам файл существует на диске.
 */
import { test, expect } from '@playwright/test';
import fs from 'fs';
import {
  apiCreateSession,
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
  const baseName = `${PREFIX}files`;
  await apiCreateSession(request, baseName);
  SESSION = fullSessionName(tmuxPrefix, baseName);
});

/** Пути, созданные тестами — удаляются в afterAll, чтобы не копить мусор. */
const created: string[] = [];

test.afterAll(async ({ request }) => {
  for (const p of created) {
    try {
      fs.unlinkSync(p);
    } catch {
      /* файла может не быть — тест мог упасть до записи */
    }
  }
  await cleanupE2ESessions(request, fullSessionName(tmuxPrefix, PREFIX));
  await cleanupE2ESessions(request, PREFIX);
});

/** Открывает приложение и подключается к сессии (терминал становится живым). */
async function openSession(page: any) {
  await page.goto('/');
  const item = page.locator(`[data-session="${SESSION}"]`);
  await expect(item).toBeVisible({ timeout: 10_000 });
  await item.click();
  await expect(page.locator('#window-bar')).toBeVisible({ timeout: 10_000 });
}

/**
 * Бросает файл в #terminal синтетическим DragEvent с настоящим DataTransfer.
 *
 * `dragenter` шлётся отдельно — на нём проверяется подсветка зоны броска.
 */
async function dropFile(page: any, name: string, mime: string, content: string) {
  await page.evaluate(
    ({ name, mime, content }) => {
      const term = document.getElementById('terminal')!;
      const dt = new DataTransfer();
      dt.items.add(new File([content], name, { type: mime }));
      term.dispatchEvent(
        new DragEvent('dragenter', { dataTransfer: dt, bubbles: true, cancelable: true }),
      );
      term.dispatchEvent(
        new DragEvent('drop', { dataTransfer: dt, bubbles: true, cancelable: true }),
      );
    },
    { name, mime, content },
  );
}

/** Вставляет файл из буфера синтетическим ClipboardEvent в область терминала. */
async function pasteFile(page: any, name: string, mime: string, content: string) {
  await page.evaluate(
    ({ name, mime, content }) => {
      const term = document.getElementById('terminal')!;
      const dt = new DataTransfer();
      dt.items.add(new File([content], name, { type: mime }));
      // target — сам #terminal: модуль проверяет вложенность через contains,
      // так что реальный helper-textarea xterm тоже подошёл бы.
      term.dispatchEvent(
        new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }),
      );
    },
    { name, mime, content },
  );
}

/** Достаёт абсолютный путь из текста тоста «Вставлено: <path>». */
function pathFromToast(text: string): string {
  const raw = text.replace(/^.*Вставлено:\s*/, '').trim();
  // Путь с пробелами приходит закавыченным — для fs кавычки надо снять.
  const path = raw.replace(/^'(.*)'$/, '$1');
  created.push(path);
  return path;
}

test.describe('Передача файлов в терминал', () => {
  test('drag&drop файла кладёт его в .forge/pasted и показывает путь', async ({ page }) => {
    await openSession(page);

    // Имя уникально на прогон: при повторе бэкенд развёл бы коллизию в
    // `dropped-2.txt`, и тест бы «упал» на собственных артефактах прошлого раза.
    const name = `dropped-${Date.now()}.txt`;
    await dropFile(page, name, 'text/plain', 'dropped-content');

    const toast = page.locator('.paste-toast');
    await expect(toast).toContainText('Вставлено:', { timeout: 10_000 });

    const filePath = pathFromToast((await toast.textContent()) || '');
    expect(filePath).toContain('/.forge/pasted/');
    expect(filePath.endsWith(name)).toBeTruthy();
    expect(fs.readFileSync(filePath, 'utf8')).toBe('dropped-content');
  });

  test('dragenter подсвечивает зону броска, drop её гасит', async ({ page }) => {
    await openSession(page);

    await page.evaluate(() => {
      const term = document.getElementById('terminal')!;
      const dt = new DataTransfer();
      dt.items.add(new File(['x'], 'hover.png', { type: 'image/png' }));
      term.dispatchEvent(
        new DragEvent('dragenter', { dataTransfer: dt, bubbles: true, cancelable: true }),
      );
    });

    await expect(page.locator('.terminal-dropzone')).toBeVisible({ timeout: 5_000 });

    await page.evaluate(() => {
      const term = document.getElementById('terminal')!;
      const dt = new DataTransfer();
      dt.items.add(new File(['x'], 'hover.png', { type: 'image/png' }));
      term.dispatchEvent(
        new DragEvent('drop', { dataTransfer: dt, bubbles: true, cancelable: true }),
      );
    });

    await expect(page.locator('.terminal-dropzone')).toBeHidden({ timeout: 5_000 });

    // Бросок настоящий — файл записался; регистрируем его на удаление.
    const toast = page.locator('.paste-toast');
    await expect(toast).toContainText('Вставлено:', { timeout: 10_000 });
    pathFromToast((await toast.textContent()) || '');
  });

  test('вставка из буфера безымянного скриншота даёт pasted_<дата>.png', async ({ page }) => {
    await openSession(page);

    // Так скриншот приезжает из системного буфера: имя обезличенное, тип png.
    await pasteFile(page, 'image.png', 'image/png', 'fake-png-bytes');

    const toast = page.locator('.paste-toast');
    await expect(toast).toContainText('Вставлено:', { timeout: 10_000 });

    const filePath = pathFromToast((await toast.textContent()) || '');
    // Суффикс -2 допустим: повторный прогон в ту же секунду упрётся в
    // разведение коллизий на бэкенде.
    expect(filePath).toMatch(/\/\.forge\/pasted\/pasted_\d{8}_\d{6}(-\d+)?\.png$/);
    expect(fs.existsSync(filePath)).toBeTruthy();
  });

  test('пустой файл (папка) отвергается с понятным сообщением', async ({ page }) => {
    await openSession(page);

    await dropFile(page, 'some-folder', '', '');

    await expect(page.locator('.paste-toast')).toContainText('не поддерживается', {
      timeout: 10_000,
    });
  });
});
