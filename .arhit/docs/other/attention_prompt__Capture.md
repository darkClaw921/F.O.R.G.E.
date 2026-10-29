# attention_prompt::Capture

Read-only bounded tmux transport for orange attention. Production reads the current server; test transport accepts a private socket and forces /dev/null configuration so user hooks are not executed. No method sends terminal keys or chat input. Metadata envelopes distinguish current visible pane output from copy-mode history and changed pane processes.
