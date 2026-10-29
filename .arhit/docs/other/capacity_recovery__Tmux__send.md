# capacity_recovery::Tmux::send

Sends only the fixed Russian word продолжи literally followed by Enter through one tmux command queue to the validated physical pane ID. No active-window fallback, clipboard manipulation, clearing input or slash commands. Called only after stable detector and final fresh capture/process verification; private-socket tests verify exact UTF-8 bytes and carriage return.
