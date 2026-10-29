# agents::jsonl::prune

Bound retained historical events to the configured horizon. Keep one pre-horizon context per session and only lifecycle/command state of a task crossing the boundary. Old usage is reduced to a metadata-only activity anchor so it can preserve task continuity without charging tokens/cost outside the retained history. Original source offsets/parser state stay intact for future append reads; rewritten sources are reconstructed independently. This runs with periodic discovery and makes normalized cursor/cache size bounded by history plus small boundary state.
