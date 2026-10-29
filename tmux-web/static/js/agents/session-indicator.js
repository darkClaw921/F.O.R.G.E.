/** Tooltip for the task-backed sidebar star; no terminal-output heuristics. */
import { providerName } from './core.js';
export function generatingTooltip(session) {
    const n=typeof session.working_agent_count==='number'&&Number.isFinite(session.working_agent_count)&&session.working_agent_count>0?Math.floor(session.working_agent_count):1;
    const providers=[...new Set((Array.isArray(session.agent_providers)?session.agent_providers:[]).map(providerName))];
    const base=`В сессии «${session.name}» ${n===1?'работает агент':`работают агенты: ${n}`}${providers.length?` (${providers.join(', ')})`:''}`;
    const value=session.generating_since_secs;
    if(typeof value!=='number'||!Number.isFinite(value)||value<0)return `${base}. Активная задача из журнала агента.`;
    const secs=Math.floor(value),mins=Math.floor(secs/60),hours=Math.floor(mins/60);
    const time=hours?`${hours} ч ${mins%60} мин`:mins?`${mins} мин ${secs%60} с`:`${secs} с`;
    return `${base}. Время от начала ${n===1?'задачи':'самой ранней задачи'}: ${time}.`;
}
