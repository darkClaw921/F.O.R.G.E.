# capacity_recovery::RetryState::attempted

Consumes the current Failure hash and monotonic timestamp before any terminal write and increments the saturating attempt counter. This prevents duplicate text if tmux fails or times out after partially delivering input. Recovery never clears or overwrites user input to repair an uncertain delivery.
