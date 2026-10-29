# capacity_recovery::styled_lines

Bounded visible-screen ANSI tokenizer yielding physical Line/Cell/Style rows while preserving SGR state across rows. Supports normal reset/dim/red/basic and extended foreground/background colors; malformed or unsupported control sequences fail closed. Does not join terminal wrap rows, preserving cursor_y alignment. No raw screen text is persisted.
