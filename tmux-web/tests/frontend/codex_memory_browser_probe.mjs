// Production memory modal, fixture-only APIs; no user memory is read or changed.
const report = { checks: [], failures: [], errors: [], width: innerWidth };
const out = document.getElementById('memory-probe-result');
const ok = (condition, label) => { report.checks.push(label); if (!condition) report.failures.push(label); };
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const wait = async (fn) => { for (let i = 0; i < 100; i++) { if (fn()) return; await sleep(20); } throw new Error('Memory UI timed out'); };
window.addEventListener('error', (event) => report.errors.push(event.message));
window.addEventListener('unhandledrejection', (event) => report.errors.push(String(event.reason)));
const calls = [], alerts = [];
window.alert = (message) => alerts.push(message);
const claude = { dir: '/profiles/claude/projects/work/memory', exists: true, index: '[Claude note](note.md)', files: [{ name: 'note.md', content: 'Claude fixture' }] };
const codex = { dir: '/profiles/codex', exists: true, scope: 'global', index_name: 'memories/MEMORY.md', index: '[Rollout](rollout_summaries/example.md)', files: [
    { name: 'memories/memory_summary.md', content: 'Codex summary' },
    { name: 'memories/rollout_summaries/example.md', content: '[Index](../MEMORY.md)\n[Missing](missing.md)' },
    { name: 'agent-memory/run-phase/MEMORY.md', content: 'Phase fixture' },
] };
let codexValue = codex, delayGet = null, delayPut = null;
const nativeFetch = window.fetch.bind(window);
window.fetch = async (input, init = {}) => {
    const url = new URL(typeof input === 'string' ? input : input.url, location.href);
    if (!url.pathname.startsWith('/api/')) return nativeFetch(input, init);
    const call = { path: url.pathname, method: init.method || 'GET', query: url.searchParams, body: init.body && JSON.parse(init.body), signal: init.signal };
    calls.push(call);
    const response = (value) => new Response(JSON.stringify(value), { headers: { 'Content-Type': 'application/json' } });
    if (call.method === 'PUT') {
        if (delayPut) { const gate = delayPut; delayPut = null; await gate; }
        return response({ ok: true });
    }
    if (delayGet && delayGet.path === call.path) { const gate = delayGet; delayGet = null; await gate.promise; return response(gate.value); }
    return response(call.path === '/api/claude-memory' ? claude : codexValue);
};
try {
    if (new URLSearchParams(location.search).get('theme') === 'light') {
        const vars = { '--bg': '#f6f8fa', '--bg-elev': '#fff', '--fg': '#24292f', '--fg-dim': '#57606a', '--fg-soft': '#24292f', '--bg-input': '#f6f8fa', '--border-input': '#d0d7de', '--accent': '#0969da', '--fg-on-accent': '#fff' };
        for (const [key, value] of Object.entries(vars)) document.documentElement.style.setProperty(key, value);
    }
    const { state } = await import('/js/core/state.js');
    const { openClaudeMemoryModal } = await import('/js/sessions/claude-memory.js');
    state.currentSession = 'fixture'; state.sessions = [{ name: 'fixture', path: '/work/project/' }];
    await openClaudeMemoryModal();
    const modal = () => document.querySelector('.claude-memory-modal');
    const body = () => modal().querySelector('.claude-memory-body');
    const tab = (key) => modal().querySelector(`[data-provider="${key}"]`);
    const section = (name) => [...modal().querySelectorAll('[data-memory-file]')].find((node) => node.dataset.memoryFile === name);
    ok(calls[0].path === '/api/claude-memory' && calls[0].query.get('path') === '/work/project/', 'Claude still loads current project');
    ok(section('MEMORY.md') && section('note.md'), 'Claude index and note remain editable sections');
    section('MEMORY.md').querySelector('a').click();
    ok(!modal().querySelector('.claude-memory-back-btn').hidden, 'Claude internal links retain back navigation');
    section('note.md').querySelector('.claude-memory-edit-btn').click();
    section('note.md').querySelector('textarea').value = 'Updated Claude';
    section('note.md').querySelector('.claude-memory-edit-save').click();
    await wait(() => !modal().querySelector('textarea'));
    ok(calls.some((call) => call.method === 'PUT' && call.path === '/api/claude-memory' && call.body.file === 'note.md' && call.body.path === '/work/project/'), 'Claude edits use existing endpoint and project');
    tab('codex').click();
    await wait(() => section('agent-memory/run-phase/MEMORY.md'));
    ok(calls.at(-1).path === '/api/codex-memory' && calls.at(-1).query.size === 0, 'Codex loads global memory without project query');
    ok(modal().querySelector('.claude-memory-scope').textContent.includes('Общая память'), 'Codex scope is clearly global');
    ok(modal().querySelectorAll('.claude-memory-source').length === 2, 'Native and subagent memory have separate headings');
    const rolloutLink = section('memories/MEMORY.md').querySelector('a');
    rolloutLink.click();
    ok(!rolloutLink.classList.contains('claude-memory-link-missing') && !modal().querySelector('.claude-memory-back-btn').hidden, 'Nested native rollout links navigate inside modal');
    const links = section('memories/rollout_summaries/example.md').querySelectorAll('a');
    ok(!links[0].classList.contains('claude-memory-link-missing') && links[1].classList.contains('claude-memory-link-missing'), 'Parent-relative links resolve and absent memory stays blocked');
    section('agent-memory/run-phase/MEMORY.md').querySelector('.claude-memory-edit-btn').click();
    section('agent-memory/run-phase/MEMORY.md').querySelector('textarea').value = 'Updated phase';
    let releasePut; delayPut = new Promise((resolve) => { releasePut = resolve; });
    section('agent-memory/run-phase/MEMORY.md').querySelector('.claude-memory-edit-save').click();
    tab('claude').click();
    await wait(() => section('note.md'));
    releasePut(); await sleep(60);
    ok(calls.some((call) => call.method === 'PUT' && call.path === '/api/codex-memory' && call.body.file === 'agent-memory/run-phase/MEMORY.md' && call.body.content === 'Updated phase'), 'Codex edit saves original relative key to Codex endpoint');
    ok(section('note.md') && tab('claude').getAttribute('aria-pressed') === 'true', 'Late save completion never redraws another provider');
    let releaseGet; const getPromise = new Promise((resolve) => { releaseGet = resolve; });
    delayGet = { path: '/api/codex-memory', promise: getPromise, value: codex };
    tab('codex').click(); const pendingCall = calls.at(-1); tab('claude').click();
    await wait(() => section('note.md')); releaseGet(); await sleep(60);
    ok(pendingCall.signal.aborted && section('note.md') && !section('memories/MEMORY.md'), 'Aborted late GET cannot replace active provider even if transport resolves');
    codexValue = { ...codex, index_name: null, index: '', files: [codex.files[2]] };
    tab('codex').click(); await wait(() => section('agent-memory/run-phase/MEMORY.md'));
    ok(modal().querySelectorAll('[data-memory-file]').length === 1, 'Subagent-only memory does not invent an empty native index');
    codexValue = { ...codex, exists: false, index_name: null, files: [], truncated: true };
    tab('claude').click(); await wait(() => section('note.md')); tab('codex').click();
    await wait(() => body().textContent.includes('пока нет сохранённой памяти'));
    ok(modal().querySelectorAll('[data-memory-file]').length === 0, 'Absent Codex memory shows an explicit empty state');
    ok(body().textContent.includes('Показана часть памяти'), 'Truncation is visible even when all candidate files were omitted');
    tab('claude').click(); await wait(() => section('note.md'));
    let releaseClosed; delayGet = { path: '/api/codex-memory', promise: new Promise((resolve) => { releaseClosed = resolve; }), value: codex };
    tab('codex').click(); const closeCall = calls.at(-1);
    [...modal().querySelectorAll('.modal-actions button')].find((button) => button.textContent === 'Закрыть').click();
    releaseClosed(); await sleep(60);
    ok(closeCall.signal.aborted && !modal() && alerts.length === 0, 'Closing aborts pending read without alerts or resurrecting modal');
    codexValue = codex; await openClaudeMemoryModal(); tab('codex').click(); await wait(() => section('agent-memory/run-phase/MEMORY.md'));
    const rect = modal().getBoundingClientRect();
    ok(rect.left >= -1 && rect.right <= innerWidth + 1 && modal().scrollWidth <= modal().clientWidth + 1, 'Memory modal and nested file names fit viewport');
} catch (error) { report.failures.push(error.stack || String(error)); }
report.failures.push(...report.errors);
out.textContent = JSON.stringify(report); document.title = report.failures.length ? 'FAIL' : 'PASS';
