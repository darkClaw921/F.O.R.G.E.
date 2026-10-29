# capacity_recovery::Tmux

Private bounded terminal transport. Production uses the normal server. Tests supply an explicit private socket AND -f /dev/null on every command so user tmux config/plugins/hooks never load into the test server. Every command uses process argument vectors, a3second timeout and kill_on_drop. Validated physical pane IDs avoid active-window fallback. Capture obtains coherent pre/post metadata, and send delivers only the fixed continuation plus Enter.
