# capacity_recovery::alive

Checks CLI and pane root PID/start identities on spawn_blocking through the existing runtime identity_alive helper. Both must be confirmed true; unknown or reused/dead processes reject delivery.
