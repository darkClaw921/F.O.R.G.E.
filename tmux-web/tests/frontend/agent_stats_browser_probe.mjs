const report={checks:[],failures:[],errors:[],width:innerWidth};
const out=document.getElementById('agent-probe-result');out.textContent='LOADING';
window.addEventListener('error',e=>report.errors.push(e.message));
window.addEventListener('unhandledrejection',e=>report.errors.push(String(e.reason)));
const ok=(condition,label)=>{report.checks.push(label);if(!condition)report.failures.push(label);};
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
const wait=async(fn)=>{for(let i=0;i<80;i++){if(fn())return;await sleep(25);}throw new Error('Timed out waiting for view');};
const day='2026-10-25', at=Date.now(), calls=[];
const tokens={input:120000,output:45000,cacheRead:240000,cacheWrite:20000,reasoning:25000};
const totals={tasks:12,completed:8,aborted:1,incompleteTasks:3,requests:48,tokens,estimatedCostUsd:2.41,reportedCostUsd:0.27,unpricedRequests:2,unknownTokenRequests:1,agentTimeMs:7200000,wallTimeMs:4500000};
const base={profile:'/Users/demo/.codex',project:'/work/F.O.R.G.E.',model:'gpt-6.1-sol',startedAtMs:at-125000,lastActivityAtMs:at,endedAtMs:null,durationMs:null,elapsedMs:125000,state:'working',completed:null,tokens,estimatedCostUsd:0.27,reportedCostUsd:null,requests:4,unpricedRequests:0,bindings:[]};
const tasks=['codex','claude','opencode','copilot'].map((provider,i)=>({...base,id:`task-${i}`,nativeId:`native-${i}`,sessionId:JSON.stringify([provider,`/profiles/${provider}`,`native-${i}`]),profile:`/profiles/${provider}`,provider,model:['gpt-6.1-sol','claude-sonnet-5-5','claude-opus-5-5','gpt-6-luna'][i],tokens:i===3?null:tokens,estimatedCostUsd:i===3?null:.27,project:i===2?'/work/another project':'/work/F.O.R.G.E.'}));
let config={enabled:true,discoverDefaults:true,roots:[],historyDays:91,priceUpdates:true,priceUpdateUrl:null};
const nativeFetch=window.fetch.bind(window);
window.fetch=async(input,init={})=>{
    const url=new URL(typeof input==='string'?input:input.url,location.href);const path=url.pathname;
    if(!path.startsWith('/api/'))return nativeFetch(input,init);
    calls.push({path,method:init.method||'GET',query:url.search,body:init.body});
    const json=(value,status=200)=>new Response(JSON.stringify(value),{status,headers:{'content-type':'application/json'}});
    if(path==='/api/agents/status')return json({state:'ready',scanning:false,files:42,scannedFiles:42,historyDays:91,updatedAtMs:at,profiles:tasks.map(t=>t.profile),errors:[]});
    if(path==='/api/agents/live')return json({nowMs:at,updatedAtMs:at,workingCount:4,byProvider:{codex:1,claude:1,opencode:1,copilot:1},live:tasks,waiting:[],unknown:[]});
    if(path==='/api/agents/summary')return json({updatedAtMs:at,totals,byProvider:{codex:totals},models:tasks.map(t=>({id:t.provider,name:t.model,provider:t.provider,totals})),projects:[{id:'/work/F.O.R.G.E.',name:'/work/F.O.R.G.E.',totals}],series:Array.from({length:7},(_,i)=>({start:`2026-10-${19+i}`,totals:{...totals,estimatedCostUsd:.18+i*.07,agentTimeMs:900000+i*300000,wallTimeMs:700000+i*190000}})),pricing:{date:day,source:'https://developers.openai.com/api/docs/pricing',models:18}});
    if(path==='/api/agents/projects')return json([{id:'/work/F.O.R.G.E.',name:'/work/F.O.R.G.E.',totals},{id:'/work/another project',name:'/work/another project',totals}]);
    if(path==='/api/agents/sessions')return json(tasks.filter(t=>!url.searchParams.get('provider')||t.provider===url.searchParams.get('provider')));
    if(path==='/api/agents/bindings'){
        if(init.method==='POST'){const data=JSON.parse(init.body);const task=tasks.find(t=>t.sessionId===data.sessionId);task.bindings=[{sessionId:task.sessionId,paneId:data.paneId,tmuxSession:'1',pid:123,processStartedAt:at-200000}];return json(task.bindings[0]);}
        if(init.method==='DELETE'){const data=JSON.parse(init.body);tasks.find(t=>t.sessionId===data.sessionId).bindings=[];return new Response(null,{status:204});}
        return json({bindings:tasks.flatMap(t=>t.bindings),inventory:{available:true,updatedAtMs:at,errors:[],processes:[],panes:[{paneId:'%4',tmuxSession:'1',pid:123,candidates:[{pid:123,provider:'codex',processStartedAt:at-200000}]}]}});
    }
    if(path==='/api/agents/config'){if(init.method==='PUT')config=JSON.parse(init.body);return json(config);}
    if(path==='/api/agents/rescan')return json({state:'scanning'},202);
    if(path==='/api/agents/prices/refresh')return json({date:day,source:'https://developers.openai.com/api/docs/pricing',models:18});
    if(path==='/api/analytics/projects')return json([]);
    if(path==='/api/analytics/summary')return json({state:'ready',updatedAt:at,kpi:{humanMessages:0},series:{},range:{}});
    if(path==='/api/analytics/status')return json({state:'ready'});
    if(path==='/api/sessions')return json([{name:'1',id:'$1',path:'/work/F.O.R.G.E.',attached:0,windows:1,created:1700000000,session_group:null,origin:'local',folder_id:'__folder:/work/F.O.R.G.E.',folder_label:'F.O.R.G.E.',is_generating:tasks[0].bindings.length>0,generating_since_secs:125,working_agent_count:tasks[0].bindings.length,agent_providers:tasks[0].bindings.length?['codex']:[]}]);
    return json([]);
};

try {
    const {showAnalytics,hideAnalytics}=await import('/js/analytics/analytics.js');
    if(new URLSearchParams(location.search).get('theme')==='light'){
        const vars={'--bg':'#f6f8fa','--bg-elev':'#ffffff','--fg':'#24292f','--fg-dim':'#57606a','--border':'#d0d7de','--accent':'#0969da','--warn':'#9a6700','--danger':'#cf222e','--p0':'#cf222e','--p1':'#9a6700','--p2':'#0969da'};
        for(const[k,v]of Object.entries(vars))document.documentElement.style.setProperty(k,v);
    }
    const {buildSessionItem}=await import('/js/sessions/sessions.js');
    const {state}=await import('/js/core/state.js');
    const previousSession=state.currentSession, previousSteps=state.nextSteps;
    const attentionSession={name:'attention-probe',windows:1,attached:0,needs_attention:true,is_generating:true};
    state.currentSession=attentionSession.name;
    state.nextSteps={...previousSteps,[attentionSession.name]:{content:'An older next-step suggestion'}};
    const waiting=buildSessionItem(attentionSession),palette=document.createElement('div');
    palette.style.backgroundColor='var(--warn-bg-hover)';palette.style.color='var(--warn-soft)';
    document.body.append(waiting,palette);
    ok(waiting.classList.contains('needs-attention')&&!waiting.classList.contains('has-next-step'),'waiting question takes priority over an older next-step suggestion');
    ok(getComputedStyle(waiting).backgroundColor===getComputedStyle(palette).backgroundColor&&getComputedStyle(waiting.querySelector('.session-name')).color===getComputedStyle(palette).color,'active waiting session uses the orange warning palette');
    const answered=buildSessionItem({...attentionSession,needs_attention:false});
    ok(!answered.classList.contains('needs-attention')&&answered.classList.contains('has-next-step'),'answering clears orange and restores normal suggestion rendering');
    waiting.remove();palette.remove();state.currentSession=previousSession;state.nextSteps=previousSteps;
    localStorage.removeItem('forge.agents.filters');
    showAnalytics(undefined,'agents');
    await wait(()=>document.querySelectorAll('.agent-live-row').length===4);
    ok(document.getElementById('analytics').dataset.mode==='agents','actual shell opens Agents');
    ok(document.querySelector('.agent-live-heading h2').textContent.includes('4'),'live working count');
    ok(document.querySelectorAll('.agent-history svg').length===3,'actual SVG history charts');
    ok(document.querySelector('.agent-live-row:last-child').textContent.includes('Нет данных о токенах'),'Copilot unknown tokens stay unknown');
    ok(!/NaN|Infinity/.test(document.getElementById('analytics-agents').textContent),'no invalid numeric output');
    const select=document.querySelector('.agent-live-row select');select.value='%4';select.dispatchEvent(new Event('change',{bubbles:true}));select.focus();
    await sleep(2100);
    ok(document.activeElement===select&&select.value==='%4','poll retains manual selection and focus');
    document.querySelector('.agent-live-row button').click();
    await wait(()=>tasks[0].bindings.length===1);
    await wait(()=>document.querySelector('.agent-live-binding').textContent.includes('Вкладка 1'));
    const posted=calls.find(c=>c.path==='/api/agents/bindings'&&c.method==='POST');
    ok(JSON.parse(posted.body).sessionId===tasks[0].sessionId,'manual binding preserves tuple identity');
    document.getElementById('analytics-mode-claude').click();
    await wait(()=>document.getElementById('analytics').dataset.mode==='claude');
    const agentCalls=calls.filter(c=>c.path.startsWith('/api/agents/')).length;
    await sleep(2100);
    ok(calls.filter(c=>c.path.startsWith('/api/agents/')).length===agentCalls,'agent polling stops in Claude mode');
    ok(!document.getElementById('analytics-claude').hidden,'legacy Claude panel remains available');
    document.getElementById('analytics-mode-agents').click();
    await wait(()=>!document.getElementById('analytics-agents').hidden);
    hideAnalytics();const hiddenCalls=calls.length;await sleep(2100);ok(calls.length===hiddenCalls,'closed view sends no requests');
    showAnalytics(undefined,'agents');await sleep(150);
    ok(document.documentElement.scrollWidth<=innerWidth+1,'page fits viewport');
    ok(document.querySelector('.agent-filters').getBoundingClientRect().right<=innerWidth+1,'filters fit viewport');
    const details=document.querySelector('.agent-settings');details.open=true;details.dispatchEvent(new Event('toggle'));
    await wait(()=>!document.querySelector('.agent-settings button[type="submit"]').disabled);
    ok(document.querySelector('.agent-settings').textContent.includes('Источники и тарифы'),'settings form loads lazily');
    details.open=false;
    report.failures.push(...report.errors.map(e=>`browser error: ${e}`));
} catch(error){report.failures.push(error.stack||String(error));}
out.textContent=JSON.stringify(report);document.title=report.failures.length?'FAIL':'PASS';
