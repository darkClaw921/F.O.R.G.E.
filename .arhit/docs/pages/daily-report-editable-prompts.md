Фича: редактируемые промпты «Сводки дня».

НАЗНАЧЕНИЕ
Позволяет пользователю переопределять промпты генерации «Сводки дня» прямо из UI настроек, не пересобирая бинарь. Базовое поведение фичи «Сводка дня» не меняется — это аддитивное расширение поверх рабочей фичи (см. эпик forge-lssb).

ХРАНЕНИЕ ПРОМПТОВ
Оверрайды лежат в KV-таблице app_settings (миграция V004_app_settings.sql; репозиторий plugins/echo/src/db/repo/app_settings.rs: get/set/delete). Ключи:
- daily_report.report_prompt — константа PROMPT_KEY_REPORT (plugins/echo/src/daily_report/mod.rs)
- daily_report.suggest_prompt — константа PROMPT_KEY_SUGGEST
Значения — строковый текст промпта.

ФОЛБЭК НА ДЕФОЛТ
Если ключ отсутствует ИЛИ значение пустое после trim — используется дефолтная константа (REPORT_META_PROMPT / SUGGEST_META_PROMPT в daily_report/mod.rs, видимость pub(crate)). Таким образом «эффективный» промпт = непустой оверрайд из app_settings, иначе константа. Пустое значение = сброс к дефолту.

ПОТОК ДАННЫХ
1. UI: вкладка настроек «Сводка дня» (renderDailySummaryTab, tmux-web/static/js/settings/daily-summary-tab.js) — fieldset «Промпты генерации» с двумя textarea (report_prompt / suggest_prompt) и кнопками «Сохранить промпты» и «↺ дефолт» у каждого поля.
2. При рендере: GET /api/echo/daily-reports/prompts через getDailyReportPrompts() (api.js) → заполняет textarea эффективными значениями; *_default из ответа хранятся в замыкании для кнопок сброса.
3. Сохранение: saveDailyReportPrompts({report_prompt, suggest_prompt}) → PUT /api/echo/daily-reports/prompts (put_prompts в plugins/echo/src/routes/daily_reports.rs). Для каждого присутствующего поля: trim; пустая строка → app_settings::delete (сброс к дефолту), иначе app_settings::set. PUT возвращает актуальное состояние тем же payload, что GET.
4. Чтение при генерации: generate_report и generate_suggestions (daily_report/mod.rs) читают app_settings::get по соответствующему ключу с фильтром непустоты после trim, иначе фолбэк на константу. Следующая генерация «Сводки дня» сразу использует обновлённый промпт.

REST-КОНТРАКТ
- GET /api/echo/daily-reports/prompts (get_prompts) → { report_prompt, suggest_prompt, report_prompt_default, suggest_prompt_default }. report_prompt/suggest_prompt — эффективные значения; *_default — всегда дефолтные константы (для кнопки сброса в UI).
- PUT /api/echo/daily-reports/prompts (put_prompts) — body { report_prompt?, suggest_prompt? } (оба #[serde(default)]); отсутствующее поле не трогается, пустая строка сбрасывает оверрайд. Хелперы effective_prompt()/prompts_payload() инкапсулируют логику.

СВЯЗАННЫЕ ЭЛЕМЕНТЫ
app_settings (repo), get_prompts, put_prompts, generate_report, generate_suggestions, renderDailySummaryTab, getDailyReportPrompts, saveDailyReportPrompts.