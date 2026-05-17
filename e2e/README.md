# F.O.R.G.E. E2E Tests

End-to-end test suite for the **devforge** (tmux-web) backend + frontend, using [Playwright](https://playwright.dev/).

## Requirements

- Node.js 18+
- Rust toolchain with `cargo` (to build/run `devforge`)
- `tmux` installed on the host
- Internet access for the first run (Playwright downloads browsers)

## Setup

```bash
cd e2e
npm install
npx playwright install --with-deps chromium
```

## Running tests

```bash
# All tests (API + UI)
npm test

# API tests only
npm run test:api

# UI tests only
npm run test:ui

# With visible browser
npm run test:headed
```

## Viewing the HTML report

After each run, an HTML report is generated at `e2e/playwright-report/index.html`.

```bash
npm run report
# or open directly:
open playwright-report/index.html
```

## Architecture

```
e2e/
  playwright.config.ts    # Playwright config; starts devforge on port 17331
  fixtures/
    api-client.ts         # REST helpers for seeding and assertions
    tmux-helpers.ts       # tmux cleanup utilities
  tests/
    api/
      healthz.spec.ts     # GET /healthz
      sessions.spec.ts    # Sessions CRUD
      windows.spec.ts     # Windows CRUD (within sessions)
      tasks.spec.ts       # Tasks CRUD (beads)
      projects.spec.ts    # Projects CRUD
      todos.spec.ts       # Todos CRUD
      themes.spec.ts      # Themes CRUD
    ui/
      layout.spec.ts      # Structural: tab-bar, sidebar, containers
      sessions-ui.spec.ts # Session appear/click/kill/rename in UI
      windows-ui.spec.ts  # Window-bar: tabs, +, close
      tasks-ui.spec.ts    # Tasks kanban board
      themes-ui.spec.ts   # Theme CSS variables applied in browser
```

## Key design decisions

1. **Real backend**: The server is started via `cargo run` in `webServer` config. No mocking of the backend.
2. **Port isolation**: Tests use port `17331` (offset from the default `7331`) so they never clash with a running dev server.
3. **Tmux isolation**: All E2E sessions are created with a unique `e2e_<timestamp>_` prefix and cleaned up in `afterAll`.  The developer's own tmux sessions are never touched.
4. **API seeding**: UI tests seed required state (sessions, windows, etc.) via REST API — not by clicking through the UI — so tests are fast and not brittle.
5. **Serial mode**: All spec files run serially (`workers: 1`) to avoid tmux name collisions; tests within a file also use `test.describe.configure({ mode: 'serial' })`.
6. **Screenshots on every test**: The `use.screenshot: 'on'` config captures a screenshot for every test, visible in the HTML report.

## Troubleshooting

- **Server takes too long to start**: Increase `timeout` in `webServer` config (default 90 s). On first run, `cargo build` may take several minutes.
- **"no server running"**: Tests list sessions from a live tmux server. If tmux is not running yet, `GET /api/sessions` returns `[]` — this is correct behaviour.
- **Port conflict**: If port 17331 is already in use, change `PORT` in `playwright.config.ts`.
