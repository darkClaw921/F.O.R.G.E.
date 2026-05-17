# plugins/echo/src/claude/events.rs::Usage

Usage-объект Claude API. Поля input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens — все u64 с #[serde(default)]=0. Толерантность к отсутствию cache-полей: старые модели/режимы их не возвращают, для нас default=0 эквивалентно 'caching не использовался'.
