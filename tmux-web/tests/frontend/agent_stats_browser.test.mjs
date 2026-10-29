/** Real Chrome smoke over the production shell/modules with fixture-only APIs. */
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {existsSync,readFileSync,readdirSync,mkdtempSync,writeFileSync} from 'node:fs';
import {join,resolve,extname,sep} from 'node:path';
import {tmpdir} from 'node:os';
import {fileURLToPath} from 'node:url';
const root=resolve(fileURLToPath(new URL('../../',import.meta.url)));
const cache=join(process.env.HOME||'','Library/Caches/ms-playwright');
let chrome=process.env.CHROME_PATH;
if(!chrome&&existsSync(cache))for(const build of readdirSync(cache).filter(x=>/^chromium-\d+$/.test(x)).sort((a,b)=>Number(b.split('-')[1])-Number(a.split('-')[1]))){for(const rel of ['chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing','chrome-mac/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing','chrome-linux/chrome']){const candidate=join(cache,build,rel);if(existsSync(candidate)){chrome=candidate;break;}}if(chrome)break;}
if(!chrome||!existsSync(chrome)){console.log('[skip] agent_stats_browser: Chrome unavailable; set CHROME_PATH for browser QA.');process.exit(0);}
const output=mkdtempSync(join(tmpdir(),'forge-agent-ui-'));
const html=readFileSync(join(root,'static/index.html'),'utf8').replace(/<script\b[^>]*>[\s\S]*?<\/script>/g,'').replace('</body>','<pre id="agent-probe-result" style="display:none">PENDING</pre><script type="module" src="/tests/frontend/agent_stats_browser_probe.mjs"></script></body>');
const mime={'.js':'text/javascript','.mjs':'text/javascript','.css':'text/css','.html':'text/html','.svg':'image/svg+xml'};
const server=createServer((req,res)=>{
    const path=new URL(req.url,'http://localhost').pathname;
    if(path==='/probe.html'){res.writeHead(200,{'content-type':'text/html'});res.end(html);return;}
    const base=path.startsWith('/tests/')?root:join(root,'static');const file=resolve(base,'.'+path);
    if(!file.startsWith(base+sep)||!existsSync(file)){res.writeHead(404);res.end();return;}
    res.writeHead(200,{'content-type':mime[extname(file)]||'application/octet-stream'});res.end(readFileSync(file));
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const port=server.address().port;
try{
    for(const[name,theme,width,height]of [['dark','dark',1366,2000],['light','light',1366,2000],['mobile','dark',390,2000]]){
        const screenshot=join(output,`${name}.png`);
        const profile=mkdtempSync(join(output,`profile-${name}-`));
        const child=spawn(chrome,['--headless','--disable-gpu','--no-sandbox','--use-mock-keychain','--no-first-run','--disable-extensions','--disable-background-networking','--hide-scrollbars','--remote-debugging-port=0',`--user-data-dir=${profile}`,'about:blank'],{stdio:['ignore','ignore','pipe']});
        let stderr='';child.stderr.on('data',c=>stderr+=c);
        let socket;
        try {
            const portFile=join(profile,'DevToolsActivePort');
            let debugPort='';
            for(let i=0;i<200;i++){if(existsSync(portFile)){debugPort=readFileSync(portFile,'utf8').trim().split(/\r?\n/)[0];if(/^\d+$/.test(debugPort))break;}await new Promise(r=>setTimeout(r,50));}
            assert.match(debugPort,/^\d+$/,`Chrome debugger did not start: ${stderr.slice(-1000)}`);
            const pages=await fetch(`http://127.0.0.1:${debugPort}/json/list`).then(r=>r.json());
            const page=pages.find(p=>p.type==='page');assert.ok(page,'Chrome page target exists');
            socket=new WebSocket(page.webSocketDebuggerUrl);
            await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});});
            let seq=0;const pending=new Map();const consoleErrors=[];
            socket.addEventListener('message',event=>{const message=JSON.parse(event.data);if(message.id){const call=pending.get(message.id);if(call){pending.delete(message.id);clearTimeout(call.timeout);message.error?call.reject(new Error(JSON.stringify(message.error))):call.resolve(message.result);}}else if(message.method==='Runtime.exceptionThrown')consoleErrors.push(message.params.exceptionDetails);});
            const rpc=(method,params={})=>new Promise((resolve,reject)=>{const id=++seq;const timeout=setTimeout(()=>{pending.delete(id);reject(new Error(`CDP timeout: ${method}`));},40000);pending.set(id,{resolve,reject,timeout});socket.send(JSON.stringify({id,method,params}));});
            await rpc('Runtime.enable');await rpc('Page.enable');
            await rpc('Emulation.setDeviceMetricsOverride',{width,height,deviceScaleFactor:1,mobile:width<600});
            await rpc('Page.navigate',{url:`http://127.0.0.1:${port}/probe.html?theme=${theme}`});
            const ready=await rpc('Runtime.evaluate',{expression:`new Promise((resolve)=>{const until=Date.now()+25000;const timer=setInterval(()=>{const text=document.getElementById('agent-probe-result')?.textContent||'';if(text.startsWith('{')){clearInterval(timer);resolve(text);}else if(Date.now()>until){clearInterval(timer);resolve(JSON.stringify({checks:[],failures:['Probe did not finish: '+text],errors:[]}));}},50);})`,awaitPromise:true,returnByValue:true});
            const report=JSON.parse(ready.result.value);
            if(report.failures.length||consoleErrors.length){const dom=await rpc('Runtime.evaluate',{expression:'document.documentElement.outerHTML',returnByValue:true});writeFileSync(join(output,`debug-${name}.html`),dom.result.value);writeFileSync(join(output,`debug-${name}.json`),JSON.stringify({report,consoleErrors,stderr},null,2));}
            assert.deepEqual(report.failures,[],`${name}: ${report.failures.join('\n')}; debug ${output}`);
            assert.deepEqual(consoleErrors,[],`${name}: uncaught browser exceptions`);
            const image=await rpc('Page.captureScreenshot',{format:'png',captureBeyondViewport:false,fromSurface:true});
            writeFileSync(screenshot,Buffer.from(image.data,'base64'));
            console.log(`agent_stats_browser ${name}: ${report.checks.length} browser checks PASS (${report.width}px); screenshot ${screenshot}`);
            await rpc('Runtime.evaluate',{expression:`(()=>{const details=document.querySelector('.agent-settings');details.open=true;const add=[...details.querySelectorAll('button')].find(b=>b.textContent==='Добавить каталог');add.click();const row=details.querySelector('.agent-root-row');const inputs=row.querySelectorAll('input');inputs[0].value='/Users/demo/Library/Application Support/Profiles/second-codex/sessions';inputs[1].value='/Users/demo/Library/Application Support/Profiles/second-codex';details.scrollIntoView({block:'end'});})()`});
            await new Promise(r=>setTimeout(r,150));
            const settingsImage=await rpc('Page.captureScreenshot',{format:'png',captureBeyondViewport:false,fromSurface:true});
            const settingsPath=join(output,`${name}-settings.png`);writeFileSync(settingsPath,Buffer.from(settingsImage.data,'base64'));
            const formSize=await rpc('Runtime.evaluate',{expression:`(()=>{const n=document.querySelector('.agent-settings');return {right:n.getBoundingClientRect().right,width:innerWidth,scroll:n.scrollWidth,client:n.clientWidth};})()`,returnByValue:true});
            assert.ok(formSize.result.value.right<=width+1&&formSize.result.value.scroll<=formSize.result.value.client+1,`${name}: settings exceed viewport; ${settingsPath}`);
            console.log(`agent_stats_browser ${name}: settings viewport PASS; screenshot ${settingsPath}`);
        } finally {socket?.close();child.kill('SIGTERM');await new Promise(resolve=>{if(child.exitCode!==null)return resolve();const timeout=setTimeout(()=>{child.kill('SIGKILL');resolve();},5000);child.once('close',()=>{clearTimeout(timeout);resolve();});});}

    }
}finally{await new Promise(r=>server.close(r));}
