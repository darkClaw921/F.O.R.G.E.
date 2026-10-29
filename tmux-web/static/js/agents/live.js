/** Keyed live rows preserve manual-binding drafts and focus between snapshots. */
import {PROVIDERS, providerName, el, clear, count, cost, clock, elapsed, shortPath, tokenTotal} from './core.js';

export function createAgentLive(host, {api, doc = document, timers = globalThis, now = () => performance.now(), onChanged = () => {}}) {
    clear(host);
    const heading = el(doc,'div','agent-live-heading'); const title = el(doc,'h2'); heading.appendChild(title); host.appendChild(heading);
    const providers = el(doc,'div','agent-provider-counts'); heading.appendChild(providers);
    const badges = new Map(PROVIDERS.map(p=>{const n=el(doc,'span','agent-provider-count');providers.appendChild(n);return[p,n];}));
    const note = el(doc,'p','agent-muted','Открытые основные задачи. Подагенты входят в расход родительской задачи.'); host.appendChild(note);
    const rowsHost = el(doc,'div','agent-live-list'); host.appendChild(rowsHost);
    const empty = el(doc,'p','agent-empty','Сейчас нет работающих задач. Запустите одного из поддерживаемых агентов.'); host.appendChild(empty);
    const rows = new Map(); let visible=false, ticker=null, inventory={panes:[],available:false};
    function tick() {const at=now();for(const row of rows.values()) row.time.textContent=clock(elapsed(row.task,row.receivedAt,at));}
    function bindingOptions(row) {
        if (row.pending) return;
        row.select.disabled=!inventory.available;
        row.bind.disabled=!inventory.available||!row.select.value;
        if(doc.activeElement===row.select)return;
        const panes = new Map();
        for (const pane of inventory.panes || []) {
            if (!(pane.candidates || []).some(p=>p.provider===row.task.provider)) continue;
            const previous=panes.get(pane.paneId); if(previous) previous.names.push(pane.tmuxSession);
            else panes.set(pane.paneId,{id:pane.paneId,names:[pane.tmuxSession]});
        }
        const options=[...panes.values()].map(p=>[p.id,`Вкладка ${[...new Set(p.names)].join(', ')} · панель ${p.id}`]);
        const signature=JSON.stringify(options); if(row.signature!==signature) {
            const saved=row.select.value;clear(row.select);const first=el(doc,'option','','Выберите панель');first.value='';row.select.appendChild(first);
            for(const[value,label]of options){const o=el(doc,'option','',label);o.value=value;row.select.appendChild(o);}
            row.select.value=options.some(([v])=>v===saved)?saved:'';row.signature=signature;
        }
        row.select.disabled=!inventory.available;
        row.bind.disabled=!inventory.available||!row.select.value;
    }
    function createRow(task) {
        const root=el(doc,'article','agent-live-row');root.dataset.taskId=task.id;
        const identity=el(doc,'div','agent-live-identity'); const agent=el(doc,'strong'); const state=el(doc,'span','agent-task-state');identity.appendChild(agent);identity.appendChild(state);root.appendChild(identity);
        const context=el(doc,'div','agent-live-context');const project=el(doc,'span','agent-live-project');const model=el(doc,'span','agent-muted');context.appendChild(project);context.appendChild(model);root.appendChild(context);
        const measures=el(doc,'div','agent-live-measures');const time=el(doc,'time','agent-live-clock');const usage=el(doc,'span','agent-muted');measures.appendChild(time);measures.appendChild(usage);root.appendChild(measures);
        const binding=el(doc,'div','agent-live-binding');const bound=el(doc,'span','agent-muted');binding.appendChild(bound);
        const controls=el(doc,'div','agent-binding-controls');const select=el(doc,'select','agent-input');select.setAttribute('aria-label',`Панель для ${providerName(task.provider)}`);
        const bind=el(doc,'button','agent-button','Привязать');bind.type='button';const unbind=el(doc,'button','agent-button','Отвязать');unbind.type='button';
        controls.appendChild(select);controls.appendChild(bind);controls.appendChild(unbind);binding.appendChild(controls);
        const error=el(doc,'span','agent-error');error.hidden=true;error.setAttribute('role','alert');binding.appendChild(error);root.appendChild(binding);
        const row={root,agent,state,project,model,time,usage,bound,select,bind,unbind,error,task,receivedAt:now(),signature:null,pending:false};
        select.addEventListener('change',()=>{bind.disabled=!select.value||row.pending||!inventory.available;});
        async function action(remove) {
            if(row.pending||!remove&&!select.value)return;
            row.pending=true;bind.disabled=true;unbind.disabled=true;select.disabled=true;error.hidden=true;
            try{if(remove)await api.unbind(row.task.sessionId);else await api.bind(row.task.sessionId,select.value);if(visible)onChanged();}
            catch(e){if(visible){error.textContent=e.message||String(e);error.hidden=false;}}
            finally{row.pending=false;unbind.disabled=false;bindingOptions(row);}
        }
        bind.addEventListener('click',()=>action(false));unbind.addEventListener('click',()=>action(true));
        return row;
    }
    return {
        update(snapshot, bindingState, query={}) {
            inventory=bindingState&&bindingState.inventory||{panes:[],available:false};
            title.textContent=`Сейчас работают: ${count(snapshot.workingCount)}`;
            for(const[p,n]of badges)n.textContent=`${providerName(p)} ${count(snapshot.byProvider&&snapshot.byProvider[p]||0)}`;
            const all=[...(snapshot.live||[]),...(snapshot.waiting||[]),...(snapshot.unknown||[])];
            const shown=all.filter(t=>(!query.provider||t.provider===query.provider)&&(!query.profile||t.profile===query.profile)&&(!query.project||t.project===query.project));
            const ids=new Set(shown.map(t=>t.id));for(const[id,row]of rows){if(!ids.has(id)){row.root.remove();rows.delete(id);}}
            empty.hidden=shown.length>0;
            empty.textContent=all.length&&!shown.length?'В выбранных фильтрах текущих задач нет.':'Сейчас нет работающих задач. Запустите одного из поддерживаемых агентов.';
            for(const task of shown) {
                let row=rows.get(task.id);if(!row){row=createRow(task);rows.set(task.id,row);rowsHost.appendChild(row.root);}
                row.task=task;row.receivedAt=now();row.agent.textContent=providerName(task.provider);
                row.state.textContent=({working:'Работает',waiting:'Ожидает продолжения',unknown:'Состояние неизвестно'})[task.state]||task.state;
                row.state.title=task.state==='waiting'?'Более 10 минут без событий. При возобновлении сохраняется исходное начало задачи.':task.state==='unknown'?'Продолжение работы сейчас невозможно подтвердить.':'';
                row.root.dataset.state=task.state;row.project.textContent=shortPath(task.project);row.project.title=task.project||'Проект не указан';
                row.model.textContent=task.model||'Модель не указана';
                const total=tokenTotal(task.tokens);const usage=[total===null?'Нет данных о токенах':`${count(total)} токенов`];
                if(task.estimatedCostUsd!==null&&task.estimatedCostUsd!==undefined)usage.push(`оценка ${cost(task.estimatedCostUsd)}${task.unpricedRequests?' (неполная)':''}`);
                if(task.reportedCostUsd!==null&&task.reportedCostUsd!==undefined)usage.push(`сообщено ${cost(task.reportedCostUsd)}`);
                if(usage.length===1)usage.push('Стоимость неизвестна');row.usage.textContent=usage.join(' · ');
                const bindings=task.bindings||[];const names=[...new Set(bindings.map(b=>b.tmuxSession))];
                row.bound.textContent=names.length?`Вкладка ${names.join(', ')}`:'Не привязан к вкладке';
                row.select.hidden=bindings.length>0;row.bind.hidden=bindings.length>0;row.unbind.hidden=bindings.length===0;
                if(!inventory.available&&!bindings.length)row.bound.textContent='Панели сейчас недоступны';
                bindingOptions(row);
            }
            tick();
        },
        show(){visible=true;if(ticker===null)ticker=timers.setInterval(tick,1000);tick();},
        hide(){visible=false;if(ticker!==null)timers.clearInterval(ticker);ticker=null;},
        destroy(){this.hide();rows.clear();clear(host);},
    };
}
