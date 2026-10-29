# agents::codex_terminal::state_database

Locates the highest numeric regular state_N.sqlite in a Codex profile via bounded directory enumeration. Missing legacy database is allowed; inaccessible profile or enumeration failure prevents a new cross-profile uniqueness claim. Never creates directories or databases and never follows database-file symlinks.
