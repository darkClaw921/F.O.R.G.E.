# attention_prompt::Observer::collect

Lists all agent-pane metadata once per attention tick, captures each distinct physical pane only once, merges aliases and aggregates all-session flags. Performs no process-tree probes, lsof, journal scans or terminal input. Inventory failure retains previous flags; per-pane capture errors retain only that pane. Keeps the existing 1500ms watcher cadence.
