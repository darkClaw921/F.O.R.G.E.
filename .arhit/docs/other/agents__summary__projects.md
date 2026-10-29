# agents::summary::projects

List project shares using the same validated date/provider/profile/project filters and aggregation semantics as summary. Both ID and name are the full canonical project directory so projects with equal basenames remain distinct. Shares include task work intervals and usage metadata; no source files are opened by this query. Ordering follows token volume, requests, then stable full-path ID, and missing metrics remain nullable.
