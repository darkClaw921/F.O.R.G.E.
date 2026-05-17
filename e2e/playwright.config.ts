import { defineConfig, devices } from '@playwright/test';
import path from 'path';

const PORT = 17331;
const BASE_URL = `http://127.0.0.1:${PORT}`;

export default defineConfig({
  testDir: './tests',
  fullyParallel: false, // serialise to avoid tmux name collisions
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  reporter: [
    ['list'],
    ['html', { outputFolder: 'playwright-report', open: 'never' }],
  ],
  use: {
    baseURL: BASE_URL,
    screenshot: 'on',
    video: 'retain-on-failure',
    trace: 'on-first-retry',
    actionTimeout: 10_000,
  },
  webServer: {
    command: `cargo run --manifest-path ${path.resolve(__dirname, '../tmux-web/Cargo.toml')} -- --port ${PORT}`,
    url: `${BASE_URL}/healthz`,
    reuseExistingServer: false,
    timeout: 90_000,
    env: {
      RUST_LOG: 'warn',
    },
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
});
