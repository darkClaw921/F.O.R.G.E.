/** Persistent draft for journal sources and token-price catalogues. */
import { agentsApi } from './api.js';
import { PROVIDERS, providerName, el, clear, count } from './core.js';

export function defaultProfile(provider, path) {
    const value = String(path || '').replace(/\/+$/, '');
    if (provider === 'opencode') return value.endsWith('/opencode.db') ? value.slice(0, -12) : value;
    const expected = {codex:['sessions','archived_sessions'],claude:['projects'],copilot:['session-state']}[provider] || [];
    const slash = value.lastIndexOf('/');
    return expected.includes(value.slice(slash + 1)) ? value.slice(0, slash) || '/' : value;
}

/** show/hide only control loading/results; opening a form never starts polling. */
export function createAgentSettings(host, {api = agentsApi, doc = document, onSaved = () => {}} = {}) {
    const details = el(doc, 'details', 'agent-settings');
    details.open = false;
    details.appendChild(el(doc, 'summary', '', 'Источники и тарифы'));
    const form = el(doc, 'form', 'agent-settings-form'); details.appendChild(form); host.appendChild(details);
    const status = el(doc, 'p', 'agent-muted', 'Настройки применяются ко всем четырём агентам.'); status.setAttribute('role','status');
    const error = el(doc, 'p', 'agent-error'); error.hidden = true; error.setAttribute('role','alert');
    form.appendChild(status); form.appendChild(error);
    const check = (label) => {const wrap=el(doc,'label','agent-settings-check');const input=el(doc,'input');input.type='checkbox';wrap.appendChild(input);wrap.appendChild(el(doc,'span','',label));form.appendChild(wrap);return input;};
    const enabled = check('Собирать статистику агентов');
    const defaults = check('Читать стандартные каталоги CLI');
    const daysWrap=el(doc,'label','agent-field');daysWrap.appendChild(el(doc,'span','','Глубина истории, дней'));
    const days=el(doc,'input','agent-input');days.type='number';days.min='1';days.max='3660';days.value='91';daysWrap.appendChild(days);form.appendChild(daysWrap);
    const rootsTitle=el(doc,'h3','','Дополнительные каталоги');form.appendChild(rootsTitle);
    form.appendChild(el(doc,'p','agent-muted','Каталог журналов может пока не существовать: он появится после первого запуска агента. Для Codex обычные и архивные журналы используют один профиль.'));
    const rootsHost=el(doc,'div','agent-roots');form.appendChild(rootsHost);
    const add=el(doc,'button','agent-button','Добавить каталог');add.type='button';form.appendChild(add);
    const updates=check('Автоматически обновлять API-тарифы');
    const urlWrap=el(doc,'label','agent-field');urlWrap.appendChild(el(doc,'span','','Адрес каталога тарифов (HTTPS)'));
    const url=el(doc,'input','agent-input');url.type='url';url.placeholder='https://example.com/agent-prices.json';urlWrap.appendChild(url);form.appendChild(urlWrap);
    const pricing=el(doc,'p','agent-muted','Тарифы ещё не загружены.');form.appendChild(pricing);
    const priceHint=el(doc,'p','agent-muted');form.appendChild(priceHint);
    const actions=el(doc,'div','agent-settings-actions');
    const save=el(doc,'button','agent-button','Сохранить настройки');save.type='submit';save.disabled=true;
    const refresh=el(doc,'button','agent-button','Обновить тарифы');refresh.type='button';refresh.disabled=true;
    actions.appendChild(save);actions.appendChild(refresh);form.appendChild(actions);
    let visible=false, generation=0, pending=null, loaded=false, saving=false, refreshing=false;
    let saved=null, lastPricing=null;const rows=[];
    [enabled,defaults,days,add,updates,url].forEach(input=>{input.disabled=true;});
    const fail=(e)=>{error.hidden=false;error.textContent=e&&e.message||String(e);};
    function rootRow(value={provider:'codex',path:'',profile:''}) {
        const row=el(doc,'div','agent-root-row');
        const provider=el(doc,'select','agent-input');provider.setAttribute('aria-label','Агент дополнительного каталога');
        for(const p of PROVIDERS){const o=el(doc,'option','',providerName(p));o.value=p;provider.appendChild(o);}provider.value=value.provider;
        const path=el(doc,'input','agent-input');path.type='text';path.placeholder='Каталог журналов, например ~/.codex/sessions';path.setAttribute('aria-label','Каталог журналов');path.value=value.path||'';
        const profile=el(doc,'input','agent-input');profile.type='text';profile.placeholder='Профиль, например ~/.codex (можно оставить пустым)';profile.setAttribute('aria-label','Общий каталог профиля');profile.value=value.profile||'';
        const remove=el(doc,'button','agent-button','Убрать');remove.type='button';remove.setAttribute('aria-label','Убрать дополнительный каталог');
        for(const [input,label] of [[provider,'Агент'],[path,'Каталог журналов'],[profile,'Папка профиля']]) {
            const field=el(doc,'label','agent-field');field.appendChild(el(doc,'span','',label));field.appendChild(input);row.appendChild(field);
        }
        row.appendChild(remove);rootsHost.appendChild(row);
        const item={row,provider,path,profile};rows.push(item);remove.addEventListener('click',()=>{row.remove();rows.splice(rows.indexOf(item),1);});return item;
    }
    add.addEventListener('click',()=>rootRow());
    function paintPricing(value) {
        if(value) lastPricing=value;
        pricing.textContent=lastPricing ? `API-тарифы: ${lastPricing.date||'дата не указана'}, ${count(lastPricing.models)} моделей. Источник: ${lastPricing.source||'не указан'}.` : 'Нет данных о тарифах.';
        if(lastPricing&&lastPricing.error) pricing.textContent+=` Последнее обновление: ${lastPricing.error}`;
    }
    function updateRefresh() {
        const value=url.value.trim();
        const stored=!!saved&&value===String(saved.priceUpdateUrl||'');
        refresh.disabled=!loaded||refreshing||!value||!stored;
        priceHint.textContent=!value ? 'Без адреса используется встроенный или последний сохранённый каталог. Стоимость по API-тарифам — оценка, а не счёт подписки.' : !stored ? 'Сохраните адрес перед обновлением тарифов.' : 'Неудачное обновление сохраняет предыдущий каталог и оценки.';
    }
    url.addEventListener('input',updateRefresh);
    function read() {
        const historyDays=Number(days.value);
        if(!Number.isInteger(historyDays)||historyDays<1||historyDays>3660) throw new Error('Глубина истории должна быть от 1 до 3660 дней.');
        const priceUpdateUrl=url.value.trim()||null;
        if(priceUpdateUrl&&!/^https:\/\/[^\s]+$/i.test(priceUpdateUrl)) throw new Error('Для каталога тарифов укажите HTTPS-адрес.');
        const roots=rows.map(r=>{
            const path=r.path.value.trim();
            if(!path) throw new Error('Укажите каталог журналов или уберите пустую строку.');
            return {provider:r.provider.value,path,profile:r.profile.value.trim()||defaultProfile(r.provider.value,path)};
        });
        return {enabled:!!enabled.checked,discoverDefaults:!!defaults.checked,roots,historyDays,priceUpdates:!!updates.checked,priceUpdateUrl};
    }
    async function load() {
        if(!visible||loaded||pending) return pending;
        const seq=++generation;status.textContent='Загружаются настройки…';
        const request=Promise.allSettled([api.config(),api.summary({})]);pending=request;
        const [configResult,priceResult]=await request;
        if(pending===request) pending=null;
        if(!visible||seq!==generation) return;
        if(priceResult.status==='fulfilled') paintPricing(priceResult.value&&priceResult.value.pricing);
        if(configResult.status==='rejected'){fail(configResult.reason);status.textContent='Настройки не загружены.';return;}
        saved=configResult.value;enabled.checked=saved.enabled!==false;defaults.checked=saved.discoverDefaults!==false;days.value=String(saved.historyDays||91);updates.checked=saved.priceUpdates!==false;url.value=saved.priceUpdateUrl||'';
        clear(rootsHost);rows.length=0;(saved.roots||[]).forEach(rootRow);loaded=true;save.disabled=false;[enabled,defaults,days,add,updates,url].forEach(input=>{input.disabled=false;});status.textContent='Настройки применяются без перезапуска FORGE.';error.hidden=true;updateRefresh();
    }
    async function submit(event) {
        event&&event.preventDefault();if(!loaded||saving) return;
        let config;try{config=read();}catch(e){fail(e);return;}
        saving=true;save.disabled=true;error.hidden=true;const seq=generation;
        try {
            const result=await api.configure(config);saved=result||config;
            if(visible&&generation===seq){let changed=true;try{changed=JSON.stringify(read())!==JSON.stringify(config);}catch(_){}status.textContent=changed?'Отправленные настройки сохранены. В форме есть новые несохранённые изменения.':'Настройки сохранены. Индекс обновляется в фоне.';updateRefresh();}
            onSaved();
        }catch(e){if(visible&&generation===seq)fail(e);}
        finally{saving=false;save.disabled=!loaded;}
    }
    form.addEventListener('submit',submit);save.addEventListener('click',submit);
    refresh.addEventListener('click',async()=>{
        if(refresh.disabled||refreshing)return;refreshing=true;updateRefresh();error.hidden=true;const seq=generation;
        try{const value=await api.refreshPrices();if(visible&&generation===seq){paintPricing(value);status.textContent='Каталог тарифов обновлён.';}onSaved();}
        catch(e){if(visible&&generation===seq)fail(e);}
        finally{refreshing=false;updateRefresh();}
    });
    details.addEventListener('toggle',()=>{if(details.open&&visible)load();});
    return {
        show(){visible=true;if(details.open)return load();},
        hide(){visible=false;generation+=1;pending=null;},
        destroy(){this.hide();details.remove();},
        refs:{details,form,enabled,defaults,days,rootsHost,add,updates,url,pricing,priceHint,save,refresh,status,error,rows},
    };
}
