# capacity_recovery::Tmux::capture

Obtains pre-metadata, styled physical capture and post-metadata in one tmux command queue. Metadata includes pane root PID, cursor position/visibility, dimensions, copy mode, alternate-screen mode and dead state. Requires identical pre/post metadata and exact physical pane ID; capture races or missing panes fail closed. Physical rows preserve composer cursor alignment.
