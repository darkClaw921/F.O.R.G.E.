import assert from 'node:assert/strict';
import {makeDoc, makeEl} from './meet_popup_dom.mjs';
import {createAgentHistory} from '../../static/js/agents/history.js';
import {count, cost, duration} from '../../static/js/agents/core.js';

// Real renderer and SVG kit on a metadata-only document, with real focus identity.
const {doc, host} = makeDoc(['history']);
const element = (tag) => {
    const node = makeEl(tag); node.ownerDocument = doc;
    node.focus = () => {doc.activeElement = node;};
    return node;
};
doc.createElement = element;
doc.createElementNS = (_ns, tag) => element(tag);
doc.documentElement = element('html');
doc.defaultView = {getComputedStyle: () => ({getPropertyValue: (key) => ({'--accent':'blue','--warn':'orange','--fg':'white','--fg-dim':'gray','--bg-elev':'black','--border':'silver'})[key] || ''})};
host.ownerDocument = doc;
const pages = [];
const view = createAgentHistory(host, {doc,onPageChange:(offset) => pages.push(offset)});
const totals = (over = {}) => ({tasks:2,completed:1,aborted:0,incompleteTasks:1,requests:3,tokens:{input:100,output:30,cacheRead:10,cacheWrite:2,reasoning:20},estimatedCostUsd:2,reportedCostUsd:3,unpricedRequests:1,unknownTokenRequests:1,agentTimeMs:1200000,wallTimeMs:600000,...over});
const attack = '<img src=x onerror=alert(1)>';
const summary = {
    totals:totals(), pricing:{date:'2026-10-25'},
    models:Array.from({length:13},(_,i)=>({id:`model-${i}`,name:i === 0 ? attack : `model-${i}`,provider:'codex',totals:totals()})),
    projects:[{id:'/work/<project>',name:'/work/<project>',totals:totals()}],
    series:[{start:'2026-10-24',totals:totals()},{start:'2026-10-25',totals:totals({tokens:null,estimatedCostUsd:null})}],
};
const task = (over = {}) => ({id:'task-1',nativeId:'session-long-identifier',provider:'codex',project:'/work/project',model:attack,startedAtMs:Date.parse('2026-10-25T10:00:00Z'),state:'ended',completed:true,durationMs:60000,elapsedMs:60000,tokens:totals().tokens,estimatedCostUsd:2,reportedCostUsd:3,unpricedRequests:0,...over});
const rows=[task(),task({id:'task-2',provider:'copilot',completed:null,state:'unknown',tokens:null,estimatedCostUsd:null,reportedCostUsd:null,durationMs:null,elapsedMs:1000})];
view.update(summary,rows,{offset:0,limit:2});
const metrics=host.querySelectorAll('.agent-history-metric');
const metric = (label) => metrics.find(n=>n.querySelector('dt').textContent === label).querySelector('dd').textContent;
assert.equal(metric('Токены'),count(142),'reasoning is already inside output');
assert.equal(metric('Сумма времени агентов'),duration(1200000));
assert.equal(metric('Время с учётом параллельной работы'),duration(600000));
assert.equal(metric('Оценка по API-тарифам'),cost(2));
assert.equal(metric('Стоимость от провайдера'),cost(3),'reported and estimated charges never become a sum');
assert.match(host.textContent,/Частичная оценка/); assert.match(host.textContent,/Copilot.*после завершения/s);
assert.match(host.textContent,/Нет данных/); assert.equal(host.querySelectorAll('img').length,0);
assert.ok(host.textContent.includes(attack),'untrusted model names remain plain text');
const allNodes = () => {const out=[];const walk=n=>{out.push(n);for(const c of n.childNodes || [])walk(c);};walk(host);return out;};
const bars = () => allNodes().filter(n=>n.getAttribute && (n.getAttribute('class') || '').split(' ').includes('fchart-bar'));
assert.equal(bars().length,2,'unknown token/cost day is omitted, never rendered as a zero bar');
assert.match(host.textContent,/Дней без данных о токенах: 1/);
for (const node of allNodes()) for (const value of Object.values(node.attributes || {})) assert.doesNotMatch(value,/NaN|Infinity/);
for (const svg of allNodes().filter(n=>n.tagName === 'svg')) assert.ok(svg.getAttribute('aria-label'));

const pagination=host.querySelector('.agent-history-pagination');
const [prev,next]=pagination.querySelectorAll('button');
assert.equal(prev.disabled,true); assert.equal(next.disabled,false); next.click();
assert.deepEqual(pages,[2]);
view.update(summary,[task()],{offset:2,limit:2});
assert.equal(prev.disabled,false);assert.equal(next.disabled,true); prev.click(); assert.deepEqual(pages,[2,0]);
prev.focus();
view.update({...summary,totals:totals({requests:4})},[task({elapsedMs:61000})],{offset:2,limit:2});
assert.equal(doc.activeElement,prev,'polling does not replace pagination controls or steal focus');
assert.equal(host.querySelector('.agent-history-pagination').querySelectorAll('button')[0],prev);

const modelSection=host.querySelector('.agent-history-breakdown');
assert.equal(modelSection.querySelector('tbody').children.length,12);
const more=modelSection.querySelector('button'); more.focus(); more.click();
assert.equal(modelSection.querySelector('tbody').children.length,13);
assert.equal(more.getAttribute('aria-expanded'),'true');
view.update(summary,rows,{offset:0,limit:2});
assert.equal(doc.activeElement,more); assert.equal(modelSection.querySelector('tbody').children.length,13,'expanded breakdown remains expanded');

view.update({totals:totals({tokens:null,estimatedCostUsd:null,reportedCostUsd:null}),series:[],models:[],projects:[]},[],{offset:0,limit:2});
assert.equal(metric('Токены'),'Нет данных');assert.equal(metric('Оценка по API-тарифам'),'Нет данных');
assert.equal(bars().length,0); assert.match(host.textContent,/За выбранный период задач нет/);
assert.equal(next.disabled,true); assert.equal(prev.disabled,true);
view.update(null,null); assert.equal(metric('Токены'),'Нет данных');
view.redraw();
for (const node of allNodes()) for (const value of Object.values(node.attributes || {})) assert.doesNotMatch(value,/NaN|Infinity/);
view.destroy(); assert.equal(host.childNodes.length,0); view.update(summary,rows); view.redraw(); assert.equal(host.childNodes.length,0);
console.log('agent_stats_history: actual renderer/SVG costs, parallel time, nulls, source text, pagination/focus and teardown PASS');
