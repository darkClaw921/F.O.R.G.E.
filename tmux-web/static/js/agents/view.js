/** Agent view controller. Fields/forms survive data polling and stale replies. */
import { agentsApi } from './api.js';
import {PROVIDERS, providerName, el, clear, count, createPoller, rangeQuery} from './core.js';
import {createAgentLive} from './live.js';
import {createAgentHistory} from './history.js';
import {createAgentSettings} from './settings.js';

export function createAgentView(host, {api = agentsApi, doc = document, timers = globalThis, onBindingsChanged = () => {}, liveFactory = createAgentLive, historyFactory = createAgentHistory, settingsFactory = createAgentSettings} = {}) {
    clear(host);
    const root = el(doc, 'div', 'agent-stats'); host.appendChild(root);
    const heading = el(doc, 'div', 'agent-heading');
    heading.appendChild(el(doc, 'h1', '', 'Агенты'));
    heading.appendChild(el(doc, 'p', 'agent-muted', 'Работа Codex, Claude, OpenCode и GitHub Copilot на этой машине.'));
    root.appendChild(heading);
    const filters = el(doc, 'div', 'agent-filters'); root.appendChild(filters);
    const select = (label, options) => {
        const wrap = el(doc, 'label', 'agent-field'); wrap.appendChild(el(doc, 'span', '', label));
        const input = el(doc, 'select', 'agent-input'); for (const [value, text] of options) { const o = el(doc, 'option', '', text); o.value = value; input.appendChild(o); }
        wrap.appendChild(input); filters.appendChild(wrap); return input;
    };
    const range = select('Период', [['today','Сегодня'],['7','7 дней'],['30','30 дней'],['91','91 день'],['custom','Свои даты']]);
    const provider = select('Агент', [['','Все агенты'], ...PROVIDERS.map(p => [p, providerName(p)])]);
    const profile = select('Профиль', [['','Все профили']]); const project = select('Проект', [['','Все проекты']]);
    const dates = el(doc, 'div', 'agent-custom-dates'); dates.hidden = true;
    const from = el(doc, 'input', 'agent-input'); from.type = 'date'; from.setAttribute('aria-label', 'Начальная дата');
    const to = el(doc, 'input', 'agent-input'); to.type = 'date'; to.setAttribute('aria-label', 'Конечная дата'); dates.appendChild(from); dates.appendChild(to); filters.appendChild(dates);
    const rescan = el(doc, 'button', 'agent-button', 'Пересканировать'); rescan.type = 'button'; filters.appendChild(rescan);
    const status = el(doc, 'p', 'agent-muted'); status.setAttribute('role', 'status'); root.appendChild(status);
    const error = el(doc, 'p', 'agent-error'); error.hidden = true; error.setAttribute('role','alert'); root.appendChild(error);
    const liveHost = el(doc, 'section', 'agent-live-host'); root.appendChild(liveHost);
    const historyHost = el(doc, 'section', 'agent-history-host'); root.appendChild(historyHost);
    const settingsHost = el(doc, 'section', 'agent-settings-host'); root.appendChild(settingsHost);
    let visible = false, offset = 0, history = null, live = null, last = null, themeObserver = null;
    const values = [range, provider, profile, project, from, to];
    try { const saved = JSON.parse(globalThis.localStorage && globalThis.localStorage.getItem('forge.agents.filters') || '{}'); values.forEach((n,i) => {if (saved[i] !== undefined) {if ((n===profile||n===project)&&saved[i]){const o=el(doc,'option','',saved[i]);o.value=saved[i];n.appendChild(o);}n.value = saved[i];}}); } catch (_) { /* isolated storage */ }
    if (!['today','7','30','91','custom'].includes(range.value)) range.value = 'today';
    const query = () => ({...rangeQuery(range.value, from.value, to.value), ...(provider.value ? {provider:provider.value}:{}), ...(profile.value ? {profile:profile.value}:{}), ...(project.value ? {project:project.value}:{}), limit:50, offset});
    const fail = (e) => {error.hidden = false; error.textContent = e && e.message || String(e);};
    const optionList = (input, rows, all) => {
        const current = input.value; const list = [...new Map(rows.filter(([v]) => !!v)).entries()];
        const signature = JSON.stringify(list); if (input.dataset.options === signature || doc.activeElement === input) return;
        input.dataset.options = signature; clear(input); const first = el(doc, 'option', '', all); first.value = ''; input.appendChild(first);
        for (const [value, name] of list) {const o = el(doc,'option','',name); o.value = value; input.appendChild(o);}
        if (current && !list.some(([v]) => v === current)) {const o = el(doc,'option','',current); o.value = current; input.appendChild(o);}
        input.value = current;
    };
    const poll = createPoller({timers, getQuery:query, onError:fail, load:async(q,{isCurrent}) => {
        const [st,snapshot,bindings] = await Promise.all([api.status(),api.live(),api.bindings()]);
        if(!isCurrent())return null;
        const key = JSON.stringify(q) + new Date().toDateString();
        const runtimeKey=JSON.stringify(['live','waiting','unknown'].map(k=>(snapshot[k]||[]).map(t=>[t.id,t.state,t.startedAtMs,t.lastActivityAtMs])));
        const unchanged = last && last.key === key && last.runtimeKey === runtimeKey && last.st.updatedAtMs === st.updatedAtMs && !snapshot.workingCount;
        const [summary,projects,sessions] = unchanged ? [last.summary,last.projects,last.sessions] : await Promise.all([api.summary(q),api.projects({...q,project:undefined,offset:undefined,limit:undefined}),api.sessions(q)]);
        return {st,snapshot,summary,projects,sessions,bindings,q,key,runtimeKey};
    },onData:(data) => {
        last = data; error.hidden = true;
        status.textContent = data.st.state === 'disabled' ? 'Сбор статистики выключен в настройках.' : data.st.scanning ? `Загружается история: ${count(data.st.scannedFiles)} из ${count(data.st.files)} файлов. Текущие задачи уже доступны.` : `История за ${count(data.st.historyDays || 91)} дней. ${data.st.errors && data.st.errors.length ? 'Некоторые источники недоступны.' : ''}`;
        optionList(profile, (data.st.profiles || []).map(p=>[p,p]), 'Все профили');
        optionList(project, (data.projects || []).map(p=>[p.id,p.name || p.id]), 'Все проекты');
        if (live) live.update(data.snapshot, data.bindings, data.q);
        if (history) history.update(data.summary,data.sessions,{offset,limit:50});
    }});
    function changed() {
        dates.hidden = range.value !== 'custom'; offset = 0;
        try { globalThis.localStorage && globalThis.localStorage.setItem('forge.agents.filters',JSON.stringify(values.map(n=>n.value))); } catch (_) { /* no persistence */ }
        poll.change();
    }
    values.forEach(n=>n.addEventListener('change',changed));
    rescan.addEventListener('click',async()=>{rescan.disabled=true;try{await api.rescan();if(visible)poll.change();}catch(e){if(visible)fail(e);}finally{rescan.disabled=false;}});
    dates.hidden = range.value !== 'custom';
    live = liveFactory(liveHost,{api,doc,timers,onChanged:()=>{onBindingsChanged();poll.change();}});
    history = historyFactory(historyHost,{doc,onPageChange:(page)=>{offset=Math.max(0,page);poll.change();}});
    const settings = settingsFactory(settingsHost,{api,doc,onSaved:()=>{last=null;poll.change();}});
    function visibility() {if(!visible)return;if(doc.hidden){poll.hide();live.hide();settings.hide();}else{poll.show();live.show();settings.show();}}
    function redraw() {if(history)history.redraw();}
    doc.addEventListener('visibilitychange',visibility);
    return {
        show() {visible=true;host.hidden=false;visibility();redraw();if(typeof MutationObserver==='function'&&!themeObserver){themeObserver=new MutationObserver(redraw);themeObserver.observe(doc.documentElement,{attributes:true,attributeFilter:['style','class']});}globalThis.addEventListener?.('resize',redraw);},
        hide() {visible=false;poll.hide();if(live)live.hide();settings.hide();themeObserver?.disconnect();themeObserver=null;globalThis.removeEventListener?.('resize',redraw);},
        destroy() {this.hide();doc.removeEventListener('visibilitychange',visibility);if(live)live.destroy();if(history)history.destroy();settings.destroy();clear(host);},
        setLive(view) {live=view;if(last)live.update(last.snapshot,last.bindings);if(visible)live.show();},
        setHistory(view) {history=view;if(last)history.update(last.summary,last.sessions,{offset,limit:50});},
        setPage(page) {offset=Math.max(0,page);poll.change();},
        refresh() {return poll.change();},
        hosts:{live:liveHost,history:historyHost}, api, onBindingsChanged,
    };
}
