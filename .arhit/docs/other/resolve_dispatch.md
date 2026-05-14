# resolve_dispatch

Чистая функция диспетчеризации запроса с возможным ?server=<id>. Pure helper, выделен для тестирования логики try_proxy_to_remote (Phase 8 .3).

## Сигнатура
fn resolve_dispatch(q: &HashMap<String, String>, remote_mode: bool) -> DispatchDecision

## Возвращает
- DispatchDecision::Local — нет ?server / пустая строка / зарезервированное 'local'.
- DispatchDecision::Proxy(id) — есть server_id, remote_mode=true.
- DispatchDecision::LegacyRejection — есть server_id, remote_mode=false → 400.

## Зарезервированное имя 'local'
?server=local трактуется как passthrough (никакого прокси, локальная обработка). Это позволяет фронту явно указать local источник без специального case'а. Реализовано через filter в extract_server_id.

## Multiple ?server=a&server=b
HashMap<String,String> от axum хранит одно значение на ключ (последнее или первое — детальfont serde_urlencoded). Dispatcher работает с этим значением и не пытается обнаруживать дубликаты — никаких 400 за multi-value.

## Тесты
Phase 8 .3 в src/main.rs#tests — 9 тестов:
- extract_server_id_local_is_reserved, extract_server_id_local_with_whitespace_still_reserved.
- resolve_dispatch_{no_server_param,empty_server_param,reserved_local,unknown_server_in_remote_mode,legacy_mode_with_server,server_with_project_param,multi_server}.

## Использование
try_proxy_to_remote вызывает resolve_dispatch и dispatch'ит:
- Local → return None (handler продолжает локальную логику).
- LegacyRejection → return Some(Err(400)).
- Proxy(id) → выполняет remote_proxy::proxy_request.
