// tmux-web / настройки — источники данных для выбора бесед авторазбора.
//
// Три площадки, три способа получить одно и то же: список ГРУПП (типы бесед у
// VK, папки аккаунта у Telegram, виды диалогов у Битрикс24) и список отдельных
// бесед. Общая форма (`peer-picker.js`) о площадках ничего не знает и получает
// эти два источника функциями — здесь они и живут.
//
// ПОЧЕМУ НЕ В `voice-tab.js`. Секция голосовых — ОДИН модуль на три панели, и
// статический импорт трёх панельных `api.js` в него тянул бы в граф модалки
// весь VK ради вкладки Битрикса. Здесь же импорты собраны в одном месте,
// рядом с местом подключения (`modal.js`), и заменяются в тестах целиком.
//
// ПОЧЕМУ У VK ФОНОВЫЙ ПРИОРИТЕТ. Пользовательский токен VK ограничен тремя
// запросами в секунду НА ВЕСЬ КЛИЕНТ, и открытая модалка не имеет права
// съедать этот бюджет: запрос идёт через общий планировщик
// (`js/vk/ratelimit.js`) с `PRIORITY_BG` — ровно как в форме видимости.

import { getDialogs as vkGetDialogs } from '../vk/api.js';
import { peerKind } from '../vk/format.js';
import { PRIORITY_BG, vkLimiter } from '../vk/ratelimit.js';
import {
    kindLabel as vkKindLabel, VISIBLE_KINDS as VK_KINDS,
} from '../vk/visibility.js';
import { getDialogs as tgGetDialogs, listFolders as tgListFolders } from '../telegram/api.js';
import { DIR_ARCHIVE, DIR_INBOX, folderKey } from '../telegram/visibility.js';
import { getDialogs as b24GetDialogs } from '../bitrix/api.js';
import {
    filterKind, kindLabel as b24KindLabel, portalPrefs, portalPrefsPatch,
    VISIBLE_KINDS as B24_KINDS,
} from '../bitrix/visibility.js';
import { settingsPortal } from './bitrix-settings-portal.js';

/** Сколько бесед просим у плагина для списка выбора. */
export const PEER_LIST_LIMIT = 200;

/** Пометки беседы одной строкой. */
function marks(list) {
    return list.filter(Boolean).join(' · ');
}

/** Группы из общего реестра видов: ключ, название, подсказка. */
function kindsAsGroups(kinds) {
    return kinds.map((k) => ({ key: k.key, label: k.label, hint: k.hint }));
}

/**
 * VK: типы бесед и беседы.
 *
 * Тип показывается ТОТ, по которому будет судить фильтр (он выводится из
 * `peer_id`, а не из поля DTO) — иначе подпись и поведение разошлись бы на
 * первой же беседе с неожиданным `kind`.
 */
export const VK_SCOPE_SOURCE = {
    async listScopeGroups() {
        return kindsAsGroups(VK_KINDS);
    },
    async listScopePeers() {
        await vkLimiter.acquire(PRIORITY_BG);
        const resp = await vkGetDialogs({ limit: PEER_LIST_LIMIT, filter: 'all' });
        const list = Array.isArray(resp && resp.dialogs) ? resp.dialogs : [];
        return list.map((dg) => ({
            id: String(dg.id),
            title: String(dg.title || dg.id),
            marks: marks([
                vkKindLabel(peerKind(dg.id)).toLowerCase(),
                dg.muted ? 'без звука' : '',
                dg.bot ? 'бот' : '',
            ]),
        }));
    },
};

/**
 * Telegram: папки аккаунта и чаты.
 *
 * `~/inbox` и `~/archive` — не папки Telegram, а пункты нашей навигации; в
 * белом списке они значат «все неархивные» и «весь архив». Порядок тот же, что
 * в форме видимости: inbox — первым, archive — последним.
 */
export const TELEGRAM_SCOPE_SOURCE = {
    async listScopeGroups() {
        const resp = await tgListFolders();
        const folders = Array.isArray(resp && resp.folders) ? resp.folders : [];
        const out = [{ key: DIR_INBOX, label: '~/inbox — все неархивные' }];
        for (const f of folders) {
            const key = folderKey(f);
            if (!key) continue;
            const title = String(f.title || key);
            out.push({ key, label: (f.emoticon ? `${f.emoticon} ` : '') + title });
        }
        out.push({ key: DIR_ARCHIVE, label: '~/archive — архив' });
        return out;
    },
    async listScopePeers() {
        const resp = await tgGetDialogs(PEER_LIST_LIMIT, 'all');
        const list = Array.isArray(resp && resp.dialogs) ? resp.dialogs : [];
        return list.map((dg) => ({
            id: String(dg.id),
            title: String(dg.title || dg.id),
            marks: marks([
                dg.archived ? 'архив' : '',
                dg.muted ? 'без звука' : '',
                dg.bot ? 'бот' : '',
            ]),
        }));
    },
};

/**
 * Битрикс24: виды диалогов и диалоги ВЫБРАННОГО В НАСТРОЙКАХ ПОРТАЛА.
 *
 * Видов ТРИ, хотя у портала их семь: `sg`, `openline`, `copilot` и `collab`
 * сведены к «чатам» (`filterKind`). Отдельного выбора «только чаты задач»
 * поверх этого нет и быть не может: чат задачи по `DIALOG_ID` неотличим от
 * обычного, и вид `task` — такая же группа в этом же списке.
 *
 * ЧЕМ ЭТА ПЛОЩАДКА ОТЛИЧАЕТСЯ ОТ ДВУХ СОСЕДНИХ (Ф7.5). Порталов у неё
 * несколько, и скоуп авторазбора у каждого свой — он лежит не в корне
 * настроек, а в записи портала (`bitrix_portal_prefs`). Отсюда три отличия,
 * которых нет ни у VK, ни у Telegram:
 *
 *   • список диалогов запрашивается с ЯВНЫМ порталом — иначе в форме,
 *     открытой на портале B, отмечались бы чаты портала A;
 *   • сохранённый выбор читается `scopeInitial` из записи портала;
 *   • тело PATCH собирает `scopePayload` — общая форма про карту не знает.
 *
 * Портал берётся у переключателя формы настроек
 * (`bitrix-settings-portal.js`), а не у панели: настраивать можно и тот
 * портал, который сейчас не открыт.
 *
 * Тумблер `bitrix_voice_autotranscribe` при этом остаётся ГЛОБАЛЬНЫМ и уходит
 * в корень настроек как раньше — он не ссылается ни на какой диалог.
 */
export const B24_SCOPE_SOURCE = {
    async listScopeGroups() {
        return kindsAsGroups(B24_KINDS);
    },
    scopeInitial(settings) {
        const prefs = portalPrefs(settings, settingsPortal());
        return { groups: prefs.voice_scope_kinds, peers: prefs.voice_scope_peers };
    },
    scopePayload(groups, peers) {
        return portalPrefsPatch(settingsPortal(), {
            voice_scope_kinds: groups,
            voice_scope_peers: peers,
        });
    },
    async listScopePeers() {
        const resp = await b24GetDialogs({ limit: PEER_LIST_LIMIT, portal: settingsPortal() });
        const list = Array.isArray(resp && resp.dialogs) ? resp.dialogs : [];
        return list.map((dg) => ({
            id: String(dg.id),
            title: String(dg.title || dg.id),
            marks: marks([
                b24KindLabel(filterKind(dg)).toLowerCase(),
                dg.muted ? 'без звука' : '',
            ]),
        }));
    },
};
