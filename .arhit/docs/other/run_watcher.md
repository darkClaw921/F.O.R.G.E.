# run_watcher

Главный async loop фонового tasks watcher'а. Spawn'ится один раз из main(). Outer-loop: borrow текущий active path → watch_one(path) → если active_path_rx закрылся, exit. Гарантирует что watcher всегда привязан к актуальному .beads/ активного проекта.
