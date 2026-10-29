# agents::codex_terminal::evidence

For each tmux pane, produces NativeEvidence only after exact native name, project suffix and canonical cwd identify one native session across all configured Codex profiles. Reads WAL-aware READ_ONLY state databases with a 50ms lock timeout. Existing database failures fail closed across profiles; no metadata is logged, cached or serialized. Evidence is later attached to one native CLI sharing the pane TTY and ancestry; root task lifecycle still comes from journals.
