// tmux-web — Settings modal: вкладка «Встречи» (плагин forge-meet, Ф7).
//
// Экспортирует `buildMeetSettings(host, deps)` — рисует вкладку в переданный
// узел. Всё, что ходит в сеть, приходит ИНЪЕКЦИЕЙ (`deps`): так вкладка
// проверяется в голом Node, без браузера и без сервера, и так же она
// доказывает, что не ходит никуда, кроме четырёх известных роутов.
//
// ============================================================================
// ЧТО ЗДЕСЬ ВАЖНО, А ЧТО ПРОСТО ПОЛЯ
// ============================================================================
//
// 1. **БЕЙДЖ УСТРОЙСТВА.** Правило проекта: локальные модели считаются только
//    на GPU. `deps.gpu_metal === false` — не «медленнее», а «расшифровки не
//    будет»: пайплайн убивает прогон, увидевший CPU. Поэтому бейдж жёлтый и
//    стоит первым, до всех настроек: человек, у которого нет Metal, должен
//    узнать об этом раньше, чем начнёт крутить пороги VAD.
//
// 2. **МОДЕЛЬ ВИДНА ЧЕСТНО.** Скачана или нет и сколько весит — рядом с
//    выбором. Иначе «выбрал large-v3 и жду расшифровку» кончается ошибкой
//    `model_unavailable` через минуту, а не отказом сразу.
//
// 3. **СКАЧИВАНИЕ ТОЛЬКО ПО ЯВНОМУ НАЖАТИЮ.** Полтора гигабайта не качаются
//    «на всякий случай» при открытии вкладки. Прогресс приезжает кадрами
//    `model_progress` через `/ws/meet`, а не опросом.
//
// 4. **ЗНАЧЕНИЯ ИЗ ОКРУЖЕНИЯ — ТОЛЬКО ДЛЯ ЧТЕНИЯ.** Сервер помечает такие
//    поля `env_locked`, и форма их блокирует. Дай мы их редактировать —
//    получилось бы «сохранил, а работает старое», потому что переменная
//    окружения сильнее файла.
//
// 5. **СОХРАНЕНИЕ НЕ ПРИМЕНЯЕТСЯ НА ЛЕТУ, и об этом сказано прямо.** Конфиг
//    плагина неизменяем в работающем процессе (разбор — в шапке
//    `plugins/meet/src/routes/config.rs`), поэтому после сохранения вкладка
//    пишет «применится при следующем запуске devforge». Молчание здесь было бы
//    ложью: человек покрутил бы таймаут, посмотрел на идущую запись и решил,
//    что настройка не работает.
//
// ============================================================================
// РАЗМЕТКА
// ============================================================================
//
// Узлами, без `innerHTML`: значения приходят с сервера (имя бота, пути,
// сообщения об ошибках внешних бинарей), и строковая сборка здесь была бы
// точкой внедрения. Классы — те же, что у соседних вкладок
// (`notify-fieldset`, `notify-hint`, `notify-error`, `notify-actions`,
// `modal-check`), поэтому своего CSS вкладка не требует.

/** Разделы формы в порядке показа. `advanced: true` — под «Для продвинутых». */
export const MEET_GROUPS = [
    { key: 'bot', title: 'Бот во встрече', advanced: false },
    { key: 'transcribe', title: 'Расшифровка', advanced: false },
    { key: 'summary', title: 'Саммари', advanced: false },
    { key: 'cloud', title: 'Облако polza.ai', advanced: false },
    { key: 'timeouts', title: 'Автовыход', advanced: false },
    { key: 'storage', title: 'Хранилище', advanced: false },
    { key: 'limits', title: 'Параллелизм', advanced: true },
    { key: 'vad', title: 'Пороги детектора речи (VAD)', advanced: true },
];

/** Человеческие подписи полей. Ключ, которого здесь нет, подписывается собой. */
export const MEET_LABELS = {
    bot_name: 'Имя бота во встрече',
    window_mode: 'Режим окна браузера',
    mute_audio: 'Глушить звук встречи в колонках',
    persist_profile: 'Постоянный профиль браузера (вход в аккаунт сохраняется)',
    whisper_model: 'Модель whisper',
    whisper_language: 'Язык распознавания',
    whisper_timeout_secs: 'Таймаут расшифровки, с',
    pertrack_transcription: 'Расшифровывать дорожки по отдельности, где дорожка = человек',
    summary_enabled: 'Считать саммари после расшифровки',
    claude_enabled: 'Разрешать локальные LLM-запуски (claude -p / kimi -p)',
    llm_backend: 'Локальный движок саммари (облако — отдельным переключателем)',
    summary_timeout_secs: 'Таймаут саммари, с',
    summary_max_chars: 'Потолок длины транскрипта для саммари, символов',
    alone_timeout_secs: 'Бот остался один, с',
    silence_timeout_secs: 'Тишина при одном собеседнике, с',
    max_meeting_secs: 'Потолок длительности встречи, с',
    vad_on_db: 'Порог включения, дБ над шумом',
    vad_off_db: 'Порог выключения, дБ над шумом',
    vad_confirm_ms: 'Подтверждение речи, мс',
    vad_hangover_ms: 'Пауза внутри реплики, мс',
    silence_gate_dbfs: 'Порог «записана тишина», dBFS (пик тише — встреча признаётся пустой)',
    max_concurrent_recordings: 'Одновременных записей',
    max_concurrent_summaries: 'Одновременных саммари',
    max_cache_bytes: 'Дисковый бюджет записей, байт (0 — не подрезать)',
    retention_days: 'Хранить дней (0 — вечно)',
    retention_min_keep: 'Последних записей не удалять никогда',
    stt_backend: 'Где расшифровывать',
    summary_backend: 'Где считать саммари',
    cloud_stt_model: 'Облачная модель расшифровки',
    cloud_stt_window_secs: 'Окно нарезки аудио для облака, с',
    cloud_summary_model: 'Облачная модель саммари',
    cloud_base_url: 'Адрес API',
    polza_api_key: 'Ключ API (показывается маской; пустое поле убирает ключ)',
};

/** Подписи переключателей стадий. */
export const BACKEND_LABELS = {
    cloud: 'в облаке (polza.ai)',
    local: 'локально',
};

/**
 * Коды нехватки — словами. Те же коды, что у `deps::blocker_hint` в бэкенде;
 * здесь они превращаются в причину ОТКАТА у переключателя стадии.
 */
export const BLOCKER_LABELS = {
    cloud_not_configured: 'ключ API не задан',
    whisper_unavailable: 'whisper не найден',
    model_unavailable: 'модели whisper нет на диске',
    metal_unavailable: 'нет GPU (Metal)',
    ffmpeg_unavailable: 'нет ffmpeg',
    claude_cli_unavailable: 'Claude CLI не найден',
};

/**
 * Символ маски ключа.
 *
 * Тот же, что `crate::cloud::MASK_CHAR` в бэкенде. Здесь он нужен ровно затем,
 * чтобы НЕ ОТПРАВИТЬ МАСКУ ОБРАТНО: `GET` отдаёт ключ только маской, и форма,
 * которая вернёт её в `PUT`, получит 400 (бэкенд узнаёт свою маску и
 * отказывается принимать её за ключ) либо, не будь той проверки, затёрла бы
 * настоящий ключ строкой из точек.
 */
export const KEY_MASK_CHAR = '•';

/**
 * Что означает выбранная стадия ПРЯМО СЕЙЧАС, с учётом окружения.
 *
 * Отдельная чистая функция, потому что это единственное место, где «облако» и
 * «локально» превращаются в проверяемое обещание: облако без ключа и локальная
 * расшифровка без Metal одинаково НЕ ЗАПУСТЯТСЯ, и узнать об этом человек
 * обязан у переключателя, а не из упавшей встречи через час.
 *
 * @param {string} key — `stt_backend` | `summary_backend`
 * @param {string} value — `cloud` | `local`
 * @param {{cloud?: object, deps?: object, config?: object}} ctx — блоки ответа
 *        `/api/meet/status`
 * @returns {{tone: 'ok'|'warn', text: string}}
 */
export function backendNote(key, value, ctx) {
    const c = ctx || {};
    const cloud = c.cloud || {};
    const deps = c.deps || {};
    const cfg = c.config || {};
    const stage = key === 'stt_backend' ? 'Расшифровка' : 'Саммари';

    // ОТКАТ ГЕЙТА — ПЕРВЫМ ДЕЛОМ.
    //
    // Заказанный путь может быть нерабочим, и тогда бэкенд не отказывает, а
    // считает вторым (`deps::choose_route`, инцидент `forge-0s0l5`). Человек
    // обязан увидеть подмену здесь: молча она хуже честного отказа — он будет
    // думать, что платит за облако, пока считает локальный whisper.
    const isStt = key === 'stt_backend';
    const chosen = isStt ? cfg.stt_backend : cfg.summary_backend;
    const effective = isStt ? cfg.stt_effective : cfg.summary_effective;
    const why = isStt ? cfg.stt_fallback_reason : cfg.summary_fallback_reason;
    if (why && chosen === value && effective && effective !== value) {
        return {
            tone: 'warn',
            text: `${stage} ЗАКАЗАНА ${BACKEND_LABELS[value] || value}, а считается `
                + `${BACKEND_LABELS[effective] || effective}: ${BLOCKER_LABELS[why] || why}. `
                + 'Либо почините заказанный путь, либо переключите стадию явно.',
        };
    }

    if (value === 'cloud') {
        if (cloud.configured !== true) {
            return {
                tone: 'warn',
                text: `${stage} в облаке НЕ ЗАПУСТИТСЯ: ключ API не задан. `
                    + 'Впишите его в разделе «Облако polza.ai» ниже.',
            };
        }
        if (cloud.error) {
            return {
                tone: 'warn',
                text: `${stage} в облаке: polza.ai не ответил `
                    + `(${cloud.error.message || cloud.error.code || 'причина неизвестна'}). `
                    + 'Пока сервис недоступен, стадия будет падать.',
            };
        }
        const left = cloud.key && cloud.key.limit_remaining;
        if (typeof left === 'number' && left <= 0) {
            return {
                tone: 'warn',
                text: `${stage} в облаке: лимит ключа исчерпан (остаток ${left}). `
                    + 'Пополните лимит либо переключите стадию на локальный расчёт.',
            };
        }
        const model = key === 'stt_backend' ? cloud.stt_model : cloud.summary_model;
        return {
            tone: 'ok',
            text: `${stage} считается в облаке polza.ai${model ? `, модель ${model}` : ''}. `
                + 'GPU для неё не нужен, но каждая встреча стоит денег.',
        };
    }

    if (key === 'stt_backend') {
        if (deps.gpu_metal !== true) {
            return {
                tone: 'warn',
                text: 'Локальная расшифровка НЕ ЗАПУСТИТСЯ: правило проекта разрешает локальные '
                    + 'модели только на GPU (Metal), а его здесь нет. Оставьте облако.',
            };
        }
        if (deps.whisper && deps.whisper.found === false) {
            return {
                tone: 'warn',
                text: 'Локальная расшифровка НЕ ЗАПУСТИТСЯ: whisper не найден. '
                    + 'brew install whisper-cpp либо оставьте облако.',
            };
        }
        return {
            tone: 'ok',
            text: 'Расшифровка идёт на этой машине: звук никуда не уезжает, '
                + 'и пословные метки времени есть только здесь.',
        };
    }

    return {
        tone: 'ok',
        text: 'Саммари считает локальный Claude CLI: транскрипт не уходит в polza.ai.',
    };
}

/**
 * Сумма в рублях. Валюта у polza.ai одна, и подписывать её обязательно: «461»
 * без знака читается как доллары.
 */
export function formatRub(n) {
    const v = Number(n);
    if (!Number.isFinite(v)) return '—';
    // Копеечные цены за минуту (0,048 ₽) при двух знаках превратились бы в
    // «0,05 ₽» и стёрли бы разницу втрое между моделями расшифровки.
    const digits = Math.abs(v) > 0 && Math.abs(v) < 1 ? 3 : 2;
    return `${v.toFixed(digits).replace('.', ',')} ₽`;
}

/**
 * Цена модели человеческой строкой.
 *
 * **Отсутствие цены — это «цену не отдали», а НЕ «бесплатно».** Наивный разбор
 * каталога читает `pricing` верхнего уровня, где у всех записей пусто, и молча
 * показывает ноль; узнают об этом по счёту в конце месяца. Здесь цена приходит
 * готовой из `top_provider.pricing` (разбирает бэкенд), а пустое значение
 * называет себя вслух.
 */
export function modelPrice(m) {
    const p = (m && m.pricing) || null;
    if (!p) return 'цена не указана';
    if (p.stt_per_minute !== null && p.stt_per_minute !== undefined) {
        return `${formatRub(p.stt_per_minute)}/мин`;
    }
    const parts = [];
    if (p.prompt_per_million !== null && p.prompt_per_million !== undefined) {
        parts.push(`${formatRub(p.prompt_per_million)}/млн вход`);
    }
    if (p.completion_per_million !== null && p.completion_per_million !== undefined) {
        parts.push(`${formatRub(p.completion_per_million)}/млн выход`);
    }
    return parts.length ? parts.join(' · ') : 'цена не указана';
}

/** Подпись строки каталога: имя, цена и метка непригодности. */
export function modelOptionLabel(m) {
    const price = modelPrice(m);
    const broken = m && m.timestamps === false ? ' — БЕЗ ТАЙМКОДОВ' : '';
    return `${(m && m.id) || ''} — ${price}${broken}`;
}

/**
 * Пригодность облачной модели расшифровки.
 *
 * Ограничение сервиса, а не наше: таймкоды даёт ОДНА модель. Флаг `timestamps`
 * приезжает с бэкенда (`crate::cloud::stt_gives_timestamps`), чтобы имя
 * пригодной модели не появилось во фронте третьей копией.
 */
export function sttModelNote(m) {
    if (!m) {
        return {
            tone: 'warn',
            text: 'Такой модели нет в каталоге polza.ai: расшифровка в облаке упадёт на первой же '
                + 'встрече. Выберите модель из списка.',
        };
    }
    if (m.timestamps === false) {
        return {
            tone: 'warn',
            text: `${m.id} НЕ ДАЁТ ТАЙМКОДОВ ВООБЩЕ. Она дешевле и быстрее, но расшифровку нечем `
                + 'положить на таймлайн, и атрибуция спикеров сломается целиком: реплики достанутся '
                + 'не тем людям. Пригодна только модель, помеченная таймкодами.',
        };
    }
    return {
        tone: 'ok',
        text: `${m.id}: таймкоды посегментные. Пословных в облаке не даёт ни одна модель, поэтому `
            + 'сегмент с низкой уверенностью помечается как наложение, а не режется по границе слова.',
    };
}

/** Токенов во входе и выходе саммари часовой встречи — для оценки стоимости. */
export const HOUR_PROMPT_TOKENS = 30_000;
export const HOUR_COMPLETION_TOKENS = 2_000;

/**
 * Во что обойдётся саммари часовой встречи выбранной моделью, рублей.
 *
 * `null` — цены нет, и выдумывать её нельзя: «бесплатно» здесь было бы враньём.
 */
export function summaryCostEstimate(m) {
    const p = (m && m.pricing) || null;
    if (!p) return null;
    const prompt = Number(p.prompt_per_million);
    const completion = Number(p.completion_per_million);
    if (!Number.isFinite(prompt) && !Number.isFinite(completion)) return null;
    const a = Number.isFinite(prompt) ? (prompt * HOUR_PROMPT_TOKENS) / 1e6 : 0;
    const b = Number.isFinite(completion) ? (completion * HOUR_COMPLETION_TOKENS) / 1e6 : 0;
    return a + b;
}

/** Подпись модели саммари: контекст и цена часовой встречи. */
export function summaryModelNote(m) {
    if (!m) {
        return {
            tone: 'warn',
            text: 'Такой модели нет в каталоге polza.ai: саммари в облаке не посчитается.',
        };
    }
    const cost = summaryCostEstimate(m);
    const ctx = Number(m.context_length);
    const parts = [];
    if (Number.isFinite(ctx) && ctx > 0) {
        parts.push(`Контекст ${ctx.toLocaleString('ru-RU')} токенов`);
        if (ctx < HOUR_PROMPT_TOKENS) {
            return {
                tone: 'warn',
                text: `${m.id}: контекст ${ctx} токенов меньше часовой встречи (≈ ${HOUR_PROMPT_TOKENS} `
                    + 'токенов) — транскрипт придётся обрезать, и саммари потеряет середину.',
            };
        }
    }
    parts.push(cost === null
        ? 'цена не указана — сколько будет стоить саммари, сервис не сказал'
        : `саммари часовой встречи обойдётся примерно в ${formatRub(cost)}`);
    return { tone: 'ok', text: `${m.id}. ${parts.join('; ')}.` };
}

/**
 * Остаток средств и лимита ключа одной строкой.
 *
 * @param {?object} dto — тело `GET /api/meet/cloud/balance`
 * @returns {{tone: 'ok'|'warn'|'wait', text: string}}
 */
export function balanceLine(dto) {
    if (!dto) return { tone: 'wait', text: 'Остаток: спрашиваем…' };
    if (dto.configured !== true) {
        return { tone: 'warn', text: 'Остаток неизвестен: ключ API не задан.' };
    }
    if (dto.error) {
        return {
            tone: 'warn',
            text: `Остаток узнать не удалось: ${dto.error.message || dto.error.code}.`,
        };
    }
    const parts = [];
    const b = dto.balance;
    if (b) {
        parts.push(`Остаток: ${formatRub(b.amount)} (потрачено ${formatRub(b.spent)})`);
    }
    const k = dto.key;
    let tone = 'ok';
    if (k && k.limit_remaining !== null && k.limit_remaining !== undefined) {
        const reset = { daily: 'сброс ежедневно', monthly: 'сброс ежемесячно' }[k.limit_reset]
            || (k.limit_reset ? `сброс: ${k.limit_reset}` : 'период сброса неизвестен');
        const of = (k.limit === null || k.limit === undefined) ? '' : ` из ${formatRub(k.limit)}`;
        parts.push(`Лимит ключа: ${formatRub(k.limit_remaining)}${of}, ${reset}`);
        if (Number(k.limit_remaining) <= 0) tone = 'warn';
    }
    if (parts.length === 0) return { tone: 'warn', text: 'Остаток неизвестен: сервис ничего не сказал.' };
    return { tone, text: `${parts.join('. ')}.` };
}

/** Подписи режимов окна. */
export const WINDOW_MODE_LABELS = {
    offscreen: 'за экраном',
    minimized: 'свёрнутое',
    visible: 'видимое',
    headless: 'headless (без GUI)',
};

/** Человеческий размер. */
export function formatBytes(n) {
    const v = Number(n);
    if (!Number.isFinite(v) || v <= 0) return '—';
    const units = ['Б', 'КБ', 'МБ', 'ГБ'];
    let i = 0;
    let x = v;
    while (x >= 1024 && i < units.length - 1) {
        x /= 1024;
        i += 1;
    }
    return `${x >= 10 || i === 0 ? Math.round(x) : x.toFixed(1)} ${units[i]}`;
}

/**
 * Состояние вычислителя: на чём пойдёт расшифровка.
 *
 * Возвращает `{ tone, text }`, где `tone` — `ok` | `warn` | `wait`. Отдельной
 * чистой функцией, потому что это ЕДИНСТВЕННОЕ место, где правило проекта
 * «только GPU» превращается в текст для человека, и проверять его надо без
 * DOM.
 */
export function deviceBadge(deps) {
    const d = deps || {};
    if (d.probed !== true) {
        return { tone: 'wait', text: 'Проверяем окружение…' };
    }
    if (d.gpu_metal === true) {
        return { tone: 'ok', text: 'Вычислитель: GPU (Metal)' };
    }
    // НЕ «медленнее», а «не будет вовсе»: пайплайн убивает прогон, который не
    // доказал Metal, — правило проекта запрещает считать локальные модели на
    // процессоре.
    return {
        tone: 'warn',
        text: 'Вычислитель: CPU. Расшифровка НЕ ЗАПУСТИТСЯ: правило проекта '
            + 'разрешает локальные модели только на GPU (Metal).',
    };
}

/** Строка состояния одного внешнего бинаря — с подсказкой, что ставить. */
export function depLine(name, dep, hint) {
    const d = dep || {};
    if (d.found === null || d.found === undefined) {
        return { tone: 'wait', text: `${name}: проверяем…` };
    }
    if (d.found === true) {
        return {
            tone: 'ok',
            text: `${name}: ${d.version || 'найден'}${d.path ? ` (${d.path})` : ''}`,
        };
    }
    return { tone: 'warn', text: `${name}: не найден. ${hint}` };
}

/** Что именно ставить, если бинаря нет. */
export const DEP_HINTS = {
    ffmpeg: 'Без него не будет архивного файла записи; расшифровка работает. brew install ffmpeg',
    ffprobe: 'Без него не проверяется дрейф таймлайна. Ставится вместе с ffmpeg.',
    whisper: 'Без него нет расшифровки вовсе. brew install whisper-cpp',
    chrome: 'Без него бот не войдёт во встречу. Поставьте Google Chrome.',
};

function el(tag, cls, text) {
    const node = document.createElement(tag);
    if (cls) node.className = cls;
    if (text !== undefined && text !== null) node.textContent = String(text);
    return node;
}

/** Строка состояния с тоном. */
function statusLine(host, { tone, text }) {
    const node = el('div', tone === 'warn' ? 'notify-error' : 'notify-hint', text);
    node.dataset.meetTone = tone;
    host.appendChild(node);
    return node;
}

/**
 * Рисует вкладку.
 *
 * @param {object} host — узел-контейнер (очищается)
 * @param {object} deps — инъекция: `getMeetConfig`, `putMeetConfig`,
 *        `getMeetStatus`, `getModels`, `downloadModel`, `recheckDeps`
 * @returns {Promise<object>} ссылки на построенные узлы (для тестов)
 */
export async function buildMeetSettings(host, deps) {
    const api = deps || {};
    host.textContent = '';

    const refs = { fields: new Map(), host };

    let config = null;
    let status = null;
    let models = null;
    let catalogue = null;
    try {
        [config, status, models, catalogue] = await Promise.all([
            api.getMeetConfig(),
            api.getMeetStatus ? api.getMeetStatus() : null,
            api.getModels ? api.getModels() : null,
            // КАТАЛОГ ОБЛАКА — ЧЕРЕЗ `catch`, а не в общей связке отказа: это
            // поход нашего сервера в ЧУЖОЙ сервис, и молчащий polza.ai не имеет
            // права оставить человека без формы, в которой чинится ключ.
            api.getCloudModels ? api.getCloudModels().catch(() => null) : null,
        ]);
    } catch (e) {
        // Форму с умолчаниями НЕ показываем: «Сохранить» затёрло бы реальный
        // конфиг значениями, которых человек не видел. Тот же приём, что во
        // вкладке уведомлений.
        const err = el('div', 'settings-load-error');
        err.appendChild(el('div', null, `Не удалось загрузить настройки встреч: ${errText(e)}`));
        const retry = el('button', 'primary', 'Повторить');
        retry.type = 'button';
        retry.addEventListener('click', () => { void buildMeetSettings(host, api); });
        err.appendChild(retry);
        host.appendChild(err);
        refs.error = err;
        return refs;
    }

    // --- 1. Окружение: устройство, бинари, перепроверка ---------------------
    const envFs = el('fieldset', 'notify-fieldset');
    envFs.appendChild(el('legend', null, 'Окружение'));
    refs.device = statusLine(envFs, deviceBadge(status && status.deps));
    const depsHost = el('div', 'meet-deps');
    envFs.appendChild(depsHost);
    refs.deps = depsHost;

    const renderDeps = (report) => {
        depsHost.textContent = '';
        const d = report || {};
        for (const [key, hint] of Object.entries(DEP_HINTS)) {
            statusLine(depsHost, depLine(key, d[key], hint));
        }
    };
    renderDeps(status && status.deps);

    const recheck = el('button', null, 'Перепроверить');
    recheck.type = 'button';
    recheck.dataset.meetAct = 'recheck';
    recheck.addEventListener('click', async () => {
        if (typeof api.recheckDeps !== 'function') return;
        recheck.disabled = true;
        try {
            const fresh = await api.recheckDeps();
            const report = (fresh && fresh.deps) || fresh;
            renderDeps(report);
            const badge = deviceBadge(report);
            refs.device.textContent = badge.text;
            refs.device.className = badge.tone === 'warn' ? 'notify-error' : 'notify-hint';
            refs.device.dataset.meetTone = badge.tone;
        } catch (e) {
            statusLine(depsHost, { tone: 'warn', text: `Перепроверка не удалась: ${errText(e)}` });
        } finally {
            recheck.disabled = false;
        }
    });
    const envActions = el('div', 'notify-actions');
    envActions.appendChild(recheck);
    envFs.appendChild(envActions);
    host.appendChild(envFs);
    refs.recheck = recheck;

    // --- 2. Модели whisper ---------------------------------------------------
    if (models) {
        const mFs = el('fieldset', 'notify-fieldset');
        mFs.appendChild(el('legend', null, 'Модели whisper'));
        mFs.appendChild(el('div', 'notify-hint', `Каталог: ${models.models_dir || ''}`));
        const list = el('ul', 'meet-models');
        for (const m of models.models || []) {
            list.appendChild(buildModelRow(m, models, api, refs));
        }
        mFs.appendChild(list);
        refs.models = list;
        host.appendChild(mFs);
    }

    // --- 3. Настройки по разделам -------------------------------------------
    //
    // Контекст переключателей стадий: облако и окружение приезжают из
    // `/api/meet/status`, и подсказка у переключателя считается по ним.
    refs.ctx = {
        cloud: (status && status.cloud) || null,
        deps: (status && status.deps) || null,
        // Решения гейта: чем стадия посчитается ФАКТИЧЕСКИ и почему это не то,
        // что заказано.
        config: (status && status.config) || null,
    };
    refs.catalogue = catalogue;
    const byGroup = new Map();
    for (const f of (config.fields || [])) {
        if (!byGroup.has(f.group)) byGroup.set(f.group, []);
        byGroup.get(f.group).push(f);
    }

    for (const group of MEET_GROUPS) {
        const fields = byGroup.get(group.key) || [];
        if (fields.length === 0) continue;
        const fs = el('fieldset', 'notify-fieldset');
        fs.dataset.meetGroup = group.key;
        fs.appendChild(el('legend', null, group.title));
        if (group.advanced) {
            fs.dataset.meetAdvanced = '1';
            fs.appendChild(el('div', 'notify-hint',
                'Для продвинутых: значения подбираются на живых записях, наугад менять не стоит.'));
        }
        for (const f of fields) fs.appendChild(buildField(f, refs));
        if (group.key === 'cloud') buildCloudBlock(fs, api, refs);
        host.appendChild(fs);
    }

    // --- 3.5. Постоянный профиль браузера -------------------------------------
    //
    // Отдельным блоком, а не строчкой среди галок: в каталоге профиля лежит
    // ЖИВАЯ СЕССИЯ аккаунта, под которым бот входит во встречи. Настройка,
    // сохраняющая на диск учётные данные, обязана объяснять это НА МЕСТЕ, а не
    // в документации, — и обязана давать кнопку «стереть» рядом с собой.
    if (config.profile) {
        host.appendChild(buildProfileBlock(config.profile, api, refs));
    }

    // --- 4. Только для чтения -------------------------------------------------
    const ro = el('fieldset', 'notify-fieldset');
    ro.appendChild(el('legend', null, 'Пути и жёсткие ограничения'));
    ro.dataset.meetGroup = 'readonly';
    const r = config.readonly || {};
    for (const [label, value] of [
        ['Конфиг', config.config_path],
        ['Записи', r.cache_dir],
        ['База', r.db_path],
        ['Модели', r.models_dir],
        ['Одновременных прогонов whisper', r.max_concurrent_whisper],
        ['Частота дискретизации, Гц', r.audio_sample_rate],
    ]) {
        ro.appendChild(el('div', 'notify-hint', `${label}: ${value === undefined ? '—' : value}`));
    }
    if (config.file_parses === false) {
        statusLine(ro, {
            tone: 'warn',
            text: 'Файл конфига не разбирается. Сохранение заблокировано: перезапись стёрла бы '
                + 'остальные настройки. Почините файл руками.',
        });
    }
    host.appendChild(ro);

    // --- 5. Сохранение --------------------------------------------------------
    const actions = el('div', 'notify-actions');
    const save = el('button', 'primary', 'Сохранить');
    save.type = 'button';
    save.dataset.meetAct = 'save';
    save.disabled = config.file_parses === false;
    const note = el('div', 'notify-hint', '');
    note.hidden = true;

    save.addEventListener('click', async () => {
        const values = collect(refs);
        if (Object.keys(values).length === 0) {
            note.hidden = false;
            note.textContent = 'Нечего сохранять: ни одно поле не изменилось.';
            return;
        }
        save.disabled = true;
        try {
            const resp = await api.putMeetConfig({ values });
            note.hidden = false;
            const saved = (resp && resp.saved) || [];
            const locked = (resp && resp.env_locked) || [];
            const fixes = (resp && resp.fixes) || [];
            const parts = [`Сохранено полей: ${saved.length}.`];
            if (locked.length) {
                parts.push(`Заданы переменными окружения и не изменены: ${locked.join(', ')}.`);
            }
            if (fixes.length) parts.push(`Поправлено: ${fixes.join('; ')}.`);
            // Молчать об этом нельзя: человек покрутил бы таймаут, посмотрел на
            // идущую запись и решил, что настройка не работает.
            parts.push('Новые значения применятся при следующем запуске devforge; '
                + 'идущие записи не тронуты.');
            note.textContent = parts.join(' ');
            note.className = 'notify-hint';
            if (resp && resp.config) applySaved(refs, resp.config);
        } catch (e) {
            note.hidden = false;
            note.className = 'notify-error';
            note.textContent = `Не сохранилось: ${errText(e)}`;
        } finally {
            save.disabled = config.file_parses === false;
        }
    });

    actions.appendChild(save);
    host.appendChild(actions);
    host.appendChild(note);
    refs.save = save;
    refs.note = note;
    refs.config = config;
    return refs;
}

/**
 * Блок постоянного профиля браузера: предупреждение, права, кнопка сброса.
 *
 * # Почему предупреждение приходит С СЕРВЕРА
 *
 * Потому что оно описывает СВОЙСТВО ХРАНЕНИЯ, а не интерфейс: что именно
 * лежит в каталоге, знает бэкенд, и написанный здесь свой текст разъехался бы
 * с фактическим поведением в первый же день, когда поведение изменится.
 *
 * # Почему кнопка видна и при выключенной настройке
 *
 * Потому что каталог мог остаться от прежней настройки. «Выключил галку, а
 * сессия Google так и лежит на диске» — ровно то молчаливое расхождение,
 * которого плагин избегает везде.
 */
function buildProfileBlock(profile, api, refs) {
    const fs = el('fieldset', 'notify-fieldset');
    fs.dataset.meetGroup = 'profile';
    fs.appendChild(el('legend', null, 'Постоянный профиль браузера'));
    fs.appendChild(el('div', 'notify-hint',
        'Нужен там, где встреча не пускает анонимного участника (например, Google Meet '
        + 'с ограниченным доступом): бот заходит залогиненным. Вход делается один раз '
        + 'руками — запустите запись в ассистированном режиме и войдите в аккаунт в окне бота.'));
    statusLine(fs, { tone: 'warn', text: profile.warning || '' });
    fs.appendChild(el('div', 'notify-hint', `Каталог: ${profile.path || '—'}`));
    const state = profile.exists
        ? `Каталог существует, права ${profile.mode || '—'}.`
        : 'Каталога нет — бот входит одноразовым гостевым профилем.';
    const stateEl = el('div', 'notify-hint', state);
    stateEl.dataset.meetProfile = 'state';
    fs.appendChild(stateEl);

    const btn = el('button', null, 'Сбросить профиль');
    btn.type = 'button';
    btn.dataset.meetAct = 'reset-profile';
    btn.disabled = !profile.exists;
    const note = el('div', 'notify-hint', '');
    note.hidden = true;
    btn.addEventListener('click', async () => {
        btn.disabled = true;
        try {
            const resp = await api.resetMeetProfile();
            const p = (resp && resp.profile) || {};
            note.hidden = false;
            note.textContent = resp && resp.removed
                ? 'Профиль удалён: следующая встреча начнётся с чистого браузера.'
                : 'Удалять было нечего — каталога профиля нет.';
            stateEl.textContent = p.exists
                ? `Каталог существует, права ${p.mode || '—'}.`
                : 'Каталога нет — бот входит одноразовым гостевым профилем.';
            btn.disabled = !p.exists;
        } catch (e) {
            note.hidden = false;
            // 409 `busy` — не поломка, а «сейчас идёт запись»: профилем
            // прямо в эту секунду пользуется живой Chrome.
            note.textContent = (e && e.code === 'busy')
                ? 'Идёт запись — профиль занят браузером бота. Попробуйте после её окончания.'
                : `Не удалось: ${(e && (e.message || e.code)) || 'ошибка'}`;
            btn.disabled = false;
        }
    });
    const actions = el('div', 'notify-actions');
    actions.appendChild(btn);
    fs.appendChild(actions);
    fs.appendChild(note);
    refs.profileReset = btn;
    return fs;
}

/**
 * Хвост раздела «Облако»: остаток средств и состояние каталога моделей.
 *
 * # Почему баланс приезжает ОТДЕЛЬНЫМ запросом и позже формы
 *
 * Потому что это второй поход в чужой сервис. В `GET /api/meet/status` его
 * намеренно нет: статус гейтит панель плагина, и подвисший polza.ai выключил
 * бы встречи целиком. Здесь тот же принцип на уровне вкладки: форма уже
 * нарисована и работает, а строка остатка дозаполняется, когда ответ придёт.
 * Отказ — строчка, а не сломанная вкладка.
 */
function buildCloudBlock(fs, api, refs) {
    const cat = refs.catalogue;
    if (cat && cat.error) {
        statusLine(fs, {
            tone: 'warn',
            text: `Каталог моделей не приехал: ${cat.error.message || cat.error.code}. `
                + 'Модели остаются полями ввода — имя придётся вписать руками.',
        });
    } else if (cat && cat.configured === false) {
        statusLine(fs, {
            tone: 'warn',
            text: 'Каталог моделей не запрашивался: ключ API не задан. '
                + 'Впишите ключ и откройте вкладку заново.',
        });
    } else if (cat && Number.isFinite(cat.total)) {
        fs.appendChild(el('div', 'notify-hint',
            `Каталог polza.ai: ${cat.total} моделей, из них пригодных для расшифровки `
            + `${(cat.stt || []).length} и для саммари ${(cat.chat || []).length}.`));
    }

    const line = statusLine(fs, balanceLine(null));
    line.dataset.meetAct = 'balance';
    refs.balance = line;
    const paint = (info) => {
        line.textContent = info.text;
        line.className = info.tone === 'warn' ? 'notify-error' : 'notify-hint';
        line.dataset.meetTone = info.tone;
    };
    if (typeof api.getCloudBalance !== 'function') {
        paint({ tone: 'wait', text: 'Остаток: неизвестен.' });
        return;
    }
    // ОТДЕЛЬНЫЙ ПРОМИС С `catch`: этим запросом вкладку уронить нельзя.
    refs.balancePromise = (async () => {
        try {
            paint(balanceLine(await api.getCloudBalance()));
        } catch (e) {
            paint({ tone: 'warn', text: `Остаток узнать не удалось: ${errText(e)}` });
        }
    })();
}

/** Строка модели: имя, вес, состояние, кнопка скачать/удалить. */
function buildModelRow(m, models, api, refs) {
    const li = el('li', 'meet-model');
    li.dataset.meetModel = m.name;
    if (m.selected) li.dataset.meetSelected = '1';

    li.appendChild(el('span', 'meet-model-name', m.name + (m.selected ? ' (выбрана)' : '')));
    // ЧЕСТНЫЙ СТАТУС: скачана или нет и сколько весит. Без него «выбрал
    // large-v3 и жду» кончается ошибкой model_unavailable через минуту.
    li.appendChild(el(
        'span',
        'meet-model-state',
        m.downloaded
            ? `на диске, ${formatBytes(m.on_disk_bytes || m.size_bytes)}`
            : `не скачана, ${formatBytes(m.size_bytes)}`,
    ));

    const downloading = (models.downloading || []).includes(m.name);
    if (!m.downloaded) {
        const btn = el('button', null, downloading ? 'Качается…' : 'Скачать');
        btn.type = 'button';
        btn.dataset.meetAct = 'download';
        btn.disabled = downloading;
        btn.addEventListener('click', async () => {
            if (typeof api.downloadModel !== 'function') return;
            btn.disabled = true;
            btn.textContent = 'Качается…';
            try {
                await api.downloadModel(m.name);
                // Прогресс приходит кадрами `model_progress` по /ws/meet —
                // опроса здесь нет намеренно.
            } catch (e) {
                btn.disabled = false;
                btn.textContent = 'Скачать';
                li.appendChild(el('span', 'notify-error', errText(e)));
            }
        });
        li.appendChild(btn);
        refs.fields.set(`model:${m.name}`, btn);
    }
    return li;
}

/**
 * Список моделей каталога для поля — либо `null`, если списка нет.
 *
 * Стадии смотрят в РАЗНЫЕ списки: расшифровка в `stt`, саммари в `chat`.
 * Смешанный список предлагал бы выбрать генератор картинок саммаризатором.
 */
export function cloudCatalogueFor(key, catalogue) {
    if (!catalogue || catalogue.error) return null;
    const list = key === 'cloud_stt_model' ? catalogue.stt
        : key === 'cloud_summary_model' ? catalogue.chat
            : null;
    return Array.isArray(list) && list.length ? list : null;
}

/** Одно поле настроек. */
function buildField(f, refs) {
    const wrap = el('div', 'meet-field');
    wrap.dataset.meetField = f.key;
    const label = MEET_LABELS[f.key] || f.key;

    // КАТАЛОГ ОБЛАКА ПРЕВРАЩАЕТ ПОЛЕ ВВОДА В СПИСОК. Имя модели, набранное
    // руками, проверяется только на живой встрече: опечатка стоит часа записи.
    const cloudList = cloudCatalogueFor(f.key, refs.catalogue);
    let input;
    if (cloudList) {
        const row = el('label', null);
        row.appendChild(document.createTextNode(`${label} `));
        input = document.createElement('select');
        const known = cloudList.some((m) => m.id === f.value);
        if (!known) {
            // Модель из конфига, которой в каталоге нет, обязана остаться
            // видимой: иначе открытие вкладки МОЛЧА переключило бы стадию на
            // чужую модель первым же сохранением.
            const opt = document.createElement('option');
            opt.value = String(f.value === undefined || f.value === null ? '' : f.value);
            opt.textContent = `${opt.value} — нет в каталоге polza.ai`;
            opt.selected = true;
            input.appendChild(opt);
        }
        for (const m of cloudList) {
            const opt = document.createElement('option');
            opt.value = m.id;
            opt.textContent = modelOptionLabel(m);
            if (m.id === f.value) opt.selected = true;
            input.appendChild(opt);
        }
        input.value = String(f.value === undefined || f.value === null ? '' : f.value);
        row.appendChild(input);
        wrap.appendChild(row);

        const note = el('div', 'notify-hint', '');
        note.dataset.meetModelNote = f.key;
        const paint = () => {
            const picked = cloudList.find((m) => m.id === String(input.value)) || null;
            const n = f.key === 'cloud_stt_model' ? sttModelNote(picked) : summaryModelNote(picked);
            note.textContent = n.text;
            note.className = n.tone === 'warn' ? 'notify-error' : 'notify-hint';
            note.dataset.meetTone = n.tone;
        };
        paint();
        input.addEventListener('change', paint);
        wrap.appendChild(note);
        refs.modelNotes = refs.modelNotes || new Map();
        refs.modelNotes.set(f.key, note);
    } else if (f.kind === 'bool') {
        const row = el('label', 'modal-check notify-check');
        input = document.createElement('input');
        input.type = 'checkbox';
        input.checked = f.value === true;
        row.appendChild(input);
        row.appendChild(document.createTextNode(` ${label}`));
        wrap.appendChild(row);
    } else if (f.kind === 'choice') {
        const row = el('label', null);
        row.appendChild(document.createTextNode(`${label} `));
        input = document.createElement('select');
        for (const c of f.choices || []) {
            const opt = document.createElement('option');
            opt.value = c;
            opt.textContent = WINDOW_MODE_LABELS[c] || BACKEND_LABELS[c] || c;
            if (c === f.value) opt.selected = true;
            input.appendChild(opt);
        }
        input.value = String(f.value === undefined || f.value === null ? '' : f.value);
        row.appendChild(input);
        wrap.appendChild(row);
    } else {
        const row = el('label', null);
        row.appendChild(document.createTextNode(`${label} `));
        input = document.createElement('input');
        // СЕКРЕТ — ТЕКСТОВОЕ ПОЛЕ, А НЕ ЧИСЛОВОЕ: сервер присылает МАСКУ
        // (`pza_RIcg••••…`), и числовой ввод показал бы её пустой строкой. Тип
        // `password` здесь не нужен и вреден: показывать нечего — значение уже
        // замаскировано на сервере, а точки поверх точек прятали бы от
        // человека, какой именно ключ настроен.
        input.type = f.kind === 'text' || f.kind === 'secret' ? 'text' : 'number';
        if (f.min !== null && f.min !== undefined) input.min = String(f.min);
        if (f.max !== null && f.max !== undefined) input.max = String(f.max);
        if (f.kind === 'float') input.step = '0.5';
        input.value = String(f.value === undefined || f.value === null ? '' : f.value);
        row.appendChild(input);
        wrap.appendChild(row);
    }

    input.dataset.meetKey = f.key;
    input.dataset.meetKind = f.kind;

    // ПЕРЕКЛЮЧАТЕЛЬ СТАДИИ ОБЪЯСНЯЕТ СЕБЯ НА МЕСТЕ и обновляется по выбору, а
    // не после сохранения: облако без ключа и локальный расчёт без Metal не
    // работают одинаково молча.
    if (f.key === 'stt_backend' || f.key === 'summary_backend') {
        input.dataset.meetBackend = f.key === 'stt_backend' ? 'stt' : 'summary';
        const note = el('div', 'notify-hint', '');
        note.dataset.meetBackendNote = input.dataset.meetBackend;
        const paint = () => {
            const n = backendNote(f.key, String(input.value), refs.ctx);
            note.textContent = n.text;
            note.className = n.tone === 'warn' ? 'notify-error' : 'notify-hint';
            note.dataset.meetTone = n.tone;
        };
        paint();
        input.addEventListener('change', paint);
        wrap.appendChild(note);
        refs.backendNotes = refs.backendNotes || new Map();
        refs.backendNotes.set(f.key, note);
    }

    // КЛЮЧ ПОКАЗАН МАСКОЙ, И ОБ ЭТОМ СКАЗАНО. Иначе строка из точек читается
    // как «поле заполнено чем-то непонятным», и человек стирает её на всякий
    // случай — то есть выключает облако, сам того не желая.
    if (f.kind === 'secret') {
        wrap.appendChild(el('div', 'notify-hint',
            'Показана только маска — полный ключ не покидает сервер. Не трогайте поле, '
            + 'чтобы оставить ключ прежним; впишите новый, чтобы заменить; очистите, '
            + 'чтобы убрать ключ и выключить облако.'));
    }
    // ЗНАЧЕНИЕ ИЗ ОКРУЖЕНИЯ — ТОЛЬКО ДЛЯ ЧТЕНИЯ: переменная сильнее файла, и
    // редактируемое поле означало бы «сохранил, а работает старое».
    if (f.env_locked === true) {
        input.disabled = true;
        wrap.appendChild(el('div', 'notify-hint',
            `Задано переменной окружения ${f.env}; файлом не перезаписывается.`));
    } else if (f.source === 'default') {
        wrap.appendChild(el('div', 'notify-hint', 'Значение по умолчанию.'));
    }
    refs.fields.set(f.key, input);
    return wrap;
}

/** Собирает ИЗМЕНЁННЫЕ значения полей. */
function collect(refs) {
    const out = {};
    const fields = (refs.config && refs.config.fields) || [];
    for (const f of fields) {
        const input = refs.fields.get(f.key);
        if (!input || input.disabled) continue;
        let value;
        if (f.kind === 'bool') value = input.checked === true;
        else if (f.kind === 'int') value = Number.parseInt(input.value, 10);
        else if (f.kind === 'float') value = Number.parseFloat(input.value);
        else value = String(input.value);
        if (typeof value === 'number' && !Number.isFinite(value)) continue;
        if (value === f.value) continue;
        // МАСКА НАЗАД НЕ УХОДИТ. Значение секрета, в котором есть символ маски,
        // — это то, что нарисовал сервер, а не то, что ввёл человек: отправка
        // такого значения даёт 400 (бэкенд узнаёт свою маску), а до появления
        // той проверки затирала бы настоящий ключ строкой из точек. Сравнения
        // `value === f.value` выше недостаточно: правка соседнего символа в
        // маске оставила бы её маской.
        if (f.kind === 'secret' && String(value).includes(KEY_MASK_CHAR)) continue;
        out[f.key] = value;
    }
    return out;
}

/** Обновляет форму ответом сервера, чтобы «изменённых полей» не осталось. */
function applySaved(refs, config) {
    refs.config = config;
    for (const f of config.fields || []) {
        const input = refs.fields.get(f.key);
        if (!input) continue;
        if (f.kind === 'bool') input.checked = f.value === true;
        else input.value = String(f.value === undefined || f.value === null ? '' : f.value);
    }
}

function errText(e) {
    if (!e) return 'неизвестная ошибка';
    if (typeof e === 'string') return e;
    return e.message || String(e);
}
