# agents::runtime::codex_service

Recognizes Codex app-server and app-server-daemon first subcommands; accounts for macOS KERN_PROCARGS2 duplicate executable path before argv0. Does not scan unrelated later arguments or prompts. Such services may host many threads across TUI and desktop clients and therefore cannot be interactive pane owners.
