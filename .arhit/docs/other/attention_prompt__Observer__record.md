# attention_prompt::Observer::record

Updates one physical pane observation. Some(true/false) records a reliable current result. None reuses a previous result only when both physical pane ID and root PID still match. Consequently an unknown pane B cannot retain a session-wide true flag after previously waiting pane A was answered, and a changed process cannot inherit old attention.
