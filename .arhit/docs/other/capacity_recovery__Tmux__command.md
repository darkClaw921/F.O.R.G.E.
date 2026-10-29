# capacity_recovery::Tmux::command

Runs tmux with an optional explicit private test socket and bounded timeout, collecting UTF-8 stdout only on success. Error messages are generic and do not contain terminal transcript or command stdout. Commands are argument vectors and never execute user text through a shell.
