import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {makeDoc,makeEl,dispatch} from './meet_popup_dom.mjs';
import {generatingTooltip} from '../../static/js/agents/session-indicator.js';

const source=readFileSync(new URL('../../static/js/agents/settings.js',import.meta.url),'utf8')
    .replace("import { agentsApi } from './api.js';",'const agentsApi = null;')
    .replace("from './core.js'",`from '${new URL('../../static/js/agents/core.js',import.meta.url).href}'`);
const {createAgentSettings,defaultProfile}=await import('data:text/javascript;base64,'+Buffer.from(source).toString('base64'));
const {doc}=makeDoc([]);const host=makeEl('div');doc.appendChild(host);
const initial={enabled:true,discoverDefaults:true,roots:[],historyDays:91,priceUpdates:true,priceUpdateUrl:'https://prices.example/catalog.json'};
const catalogue={date:'2026-10-25',source:'https://official.example/pricing',models:18};
const calls={config:0,summary:0,saved:[],refresh:0,onSaved:0};let saveFails=false,refreshFails=false;
const api={config:async()=>{calls.config++;return structuredClone(initial);},summary:async()=>{calls.summary++;return {pricing:catalogue};},configure:async(config)=>{calls.saved.push(structuredClone(config));if(saveFails)throw Error('write failed');return config;},refreshPrices:async()=>{calls.refresh++;if(refreshFails)throw Error('network failed');return {...catalogue,date:'2026-10-26'};}};
const settings=createAgentSettings(host,{api,doc,onSaved:()=>calls.onSaved++});const r=settings.refs;
settings.show();assert.equal(r.details.open,false);assert.equal(calls.config,0,'collapsed settings do not fetch');
r.details.open=true;dispatch(r.details,'toggle');await new Promise(resolve=>setImmediate(resolve));
assert.equal(calls.config,1);assert.equal(calls.summary,1);assert.equal(r.days.value,'91');assert.equal(r.save.disabled,false);
assert.match(r.pricing.textContent,/2026-10-25/);assert.match(r.pricing.textContent,/18/);
settings.show();assert.equal(calls.config,1,'live show calls never reset a loaded form');
r.add.click();assert.equal(r.rows.length,1);r.rows[0].path.value='~/.codex-other/sessions';r.rows[0].provider.value='codex';
r.enabled.checked=false;r.days.value='120';r.save.click();await new Promise(resolve=>setImmediate(resolve));
assert.deepEqual(calls.saved[0],{...initial,enabled:false,historyDays:120,roots:[{provider:'codex',path:'~/.codex-other/sessions',profile:'~/.codex-other'}]});
assert.equal(calls.onSaved,1);assert.equal(calls.saved[0].cachePath,undefined);assert.equal(calls.saved[0].configPath,undefined);
saveFails=true;r.rows[0].path.value='/edited/unsaved';r.save.click();await new Promise(resolve=>setImmediate(resolve));
assert.match(r.error.textContent,/write failed/);assert.equal(r.rows[0].path.value,'/edited/unsaved','failed save preserves draft');assert.equal(calls.onSaved,1);
refreshFails=true;const before=r.pricing.textContent;r.refresh.click();await new Promise(resolve=>setImmediate(resolve));
assert.equal(r.pricing.textContent,before,'failed refresh preserves last valid catalogue');assert.match(r.error.textContent,/network failed/);
r.url.value='https://new.example/catalog.json';dispatch(r.url,'input');assert.equal(r.refresh.disabled,true,'unsaved URL cannot refresh the wrong catalogue');
assert.match(r.priceHint.textContent,/Сохраните/);
r.days.value='0';const savedCount=calls.saved.length;r.save.click();await new Promise(resolve=>setImmediate(resolve));assert.equal(calls.saved.length,savedCount);assert.match(r.error.textContent,/1 до 3660/);
settings.hide();settings.show();assert.equal(r.rows[0].path.value,'/edited/unsaved','mode switches retain drafts');assert.equal(calls.config,1);
settings.destroy();assert.equal(host.children.length,0);

let resolveLoad;const delayed=createAgentSettings(host,{doc,api:{config:()=>new Promise(resolve=>{resolveLoad=resolve;}),summary:async()=>({pricing:catalogue})}});
delayed.refs.details.open=true;delayed.show();delayed.hide();resolveLoad(initial);await new Promise(resolve=>setImmediate(resolve));
assert.equal(delayed.refs.save.disabled,true,'a hidden load cannot paint stale settings');delayed.destroy();
assert.equal(defaultProfile('codex','/a/archived_sessions'),'/a');assert.equal(defaultProfile('claude','/a/projects'),'/a');assert.equal(defaultProfile('opencode','/a/opencode.db'),'/a');
const tooltip=generatingTooltip({name:'build',working_agent_count:2,agent_providers:['codex','claude'],generating_since_secs:3661});
assert.match(tooltip,/Codex, Claude/);assert.match(tooltip,/2/);assert.match(tooltip,/1 ч 1 мин/);assert.doesNotMatch(tooltip,/50|терминал|NaN/);
assert.doesNotMatch(generatingTooltip({name:'x',generating_since_secs:NaN}),/NaN/);

globalThis.document=doc;globalThis.window=globalThis;
const {renderAnalyticsTab}=await import('../../static/js/settings/analytics-tab.js');
const scheduled=new Map();let nextTimer=0;const opened=[];let liveCount=4;
const showcase=renderAnalyticsTab(makeEl('div'),{
    document:doc,getAnalyticsSummary:async()=>({state:'ready',kpi:{},series:{},progress:{running:false}}),
    getAgentLive:async()=>({workingCount:liveCount}),
    setTimeout:fn=>{scheduled.set(++nextTimer,fn);return nextTimer;},clearTimeout:id=>scheduled.delete(id),
    onClose:()=>opened.push('close'),showAnalytics:(...args)=>opened.push(args),openWindow:()=>null,
});
await showcase.ready;await showcase.agentsReady;
assert.match(showcase.agentCount.textContent,/4/);assert.equal(scheduled.size,1);
showcase.agentsOpenBtn.click();assert.deepEqual(opened,['close',[undefined,'agents']]);
opened.length=0;showcase.openBtn.click();assert.deepEqual(opened,['close',[undefined,'claude']]);
liveCount=2;const [timerId,callback]=[...scheduled.entries()][0];scheduled.delete(timerId);callback();await new Promise(resolve=>setImmediate(resolve));
assert.match(showcase.agentCount.textContent,/2/);
doc.hidden=true;dispatch(doc,'visibilitychange');assert.equal(scheduled.size,0,'background browser tab stops showcase polling');
doc.hidden=false;dispatch(doc,'visibilitychange');await new Promise(resolve=>setImmediate(resolve));assert.equal(scheduled.size,1,'visible tab resumes one showcase timer');
showcase.dispose();assert.equal(scheduled.size,0,'showcase live timer stops with settings dispose');
console.log('agent_stats_settings: lazy load/draft/error/save/price identity/teardown/sidebar tooltip PASS');
