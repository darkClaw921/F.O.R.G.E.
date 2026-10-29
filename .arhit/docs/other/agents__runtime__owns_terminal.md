# agents::runtime::owns_terminal

Compares a ps row controlling terminal with tmux pane TTY after removing /dev/ prefix. Rejects ? and ?? detached terminals. Combined with process-tree ancestry, prevents shared daemon and desktop-client descendants from inheriting the launching terminal ownership. Supports macOS ttysNNN and Linux pts/N notation.
