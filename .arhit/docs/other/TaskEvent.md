# TaskEvent

Enum в tasks.rs для realtime событий из beads watcher'а. Сериализуется serde с tag=kind, lowercase: {kind:'upsert', issue: <full br list issue>} либо {kind:'removed', id: '<issue-id>'}. Используется broadcast-каналом в AppState и WS-handler /ws/tasks. Upsert покрывает create+update+close (close → Upsert со status=closed), Removed — только при физическом удалении из beads БД.
