# put_prompts

PUT /api/echo/daily-reports/prompts (plugins/echo/src/routes/daily_reports.rs). Body PutPromptsBody { report_prompt?: Option<String>, suggest_prompt?: Option<String> } — оба поля #[serde(default)]. Для каждого ПРИСУТСТВУЮЩЕГО поля: trim; пустая строка → app_settings::delete(ключ) (сброс к дефолту); иначе app_settings::set(ключ, trimmed). Отсутствующее поле не трогается. После записи возвращает актуальное состояние тем же JSON, что get_prompts (через prompts_payload). 200 OK, ошибки через internal.
