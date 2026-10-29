# attention_prompt::Capture::command

Runs tmux through tokio process Command with a three-second timeout and kill_on_drop. Private test sockets always use -S plus -f /dev/null, production has neither override. Returns only successful UTF-8 stdout; failures are propagated as unknown observations and do not clear unrelated panes.
