# attention_prompt::Observer

Bounded per-physical-pane/root-PID memory of last reliable attention. A successful read replaces that pane state; a failed/copy-mode read reuses only the same pane/process state. Full pane inventories prune removed physical IDs. Complete inventory errors preserve previous session flags until a successful listing. No terminal text, prompt body, process arguments or journal content is retained.
