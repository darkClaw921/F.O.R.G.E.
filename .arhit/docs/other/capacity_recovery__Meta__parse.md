# capacity_recovery::Meta::parse

Parses the fixed private metadata row returned by tmux display-message. Requires ten fields, numeric PID/cursor/dimensions, exact boolean flags and a physical percent-prefixed numeric pane ID. Unknown or malformed metadata is never treated as an idle composer.
