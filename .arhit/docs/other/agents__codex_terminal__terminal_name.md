# agents::codex_terminal::terminal_name

Extracts only an explicit Codex TUI name when pane_title ends with exact separator and pane current-path basename. Removes one leading Unicode Braille spinner plus space; idle titles work without spinner. Rejects missing suffix, empty/oversized names and control characters. Its returned borrowed string is transient comparison input only: it does not determine Working state or retain UI content.
