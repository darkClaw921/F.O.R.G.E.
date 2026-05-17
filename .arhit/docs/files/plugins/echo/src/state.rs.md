# plugins/echo/src/state.rs

Echo plugin state. Phase 5b добавлены поля action_registry (Mutex<HashMap<message_id, ActionRegistryEntry>>) с TTL=30min для хранения Action descriptors, register_actions(message_id, actions) — evicts стейл записи и возвращает Vec<ActionDescriptor> для broadcast, find_action(action_id) — линейный поиск по всем записям с lazy-eviction. ACTION_REGISTRY_TTL_SECS=1800.
