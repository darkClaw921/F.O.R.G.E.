# tmux-web/src/attention.rs::set_generating

Метод AttentionState. Единственный писатель карты generating (финального флага is_generating).

Сигнатура: pub async fn set_generating(&self, name: &str, flag: bool).

Логика: write-lock на self.generating, map.insert(name, flag). Insert даже при false — это позволяет фронтенду различать 'никогда не видели сессию' (нет ключа) и 'видели, флаг потушен' (есть ключ со значением false). Семантика идентична существующему методу set для 'needs_attention'.

Кто вызывает: watcher_loop (Phase 3.3) после дедупликации сырых changed-сигналов от update_generation. Дедуп сворачивает linked-сессии одной session_group в общий флаг — иначе индикатор горел бы во всех вкладках группы при изменении pane хотя бы в одной.

Архитектурная роль: явная развязка raw-detector (update_generation) и финального write (set_generating). Это позволяет дедуп-фазе оперировать чистыми сигналами без побочного влияния на наблюдаемое состояние.
