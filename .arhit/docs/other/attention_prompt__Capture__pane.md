# attention_prompt::Capture::pane

Validates an exact numeric physical pane ID, queues metadata/capture/metadata in one tmux process, and requires matching pre/post identity and the expected pane root PID. Captures visible joined rows without scrollback. Normal live output is classified; dead panes clear attention. Copy mode is unknown, preserving only a known pending signal for that pane while never arming from historical menus. No text is retained after classification.
