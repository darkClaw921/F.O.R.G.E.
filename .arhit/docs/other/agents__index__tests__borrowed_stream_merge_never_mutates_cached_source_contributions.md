# agents::index::tests::borrowed_stream_merge_never_mutates_cached_source_contributions

Regression for copy-on-write replay. A start plus two streaming chunks for one native response is folded by reference; the result has one request with maximum tokens, earliest billing time and latest task activity, while the serialized original source metadata remains exactly unchanged. This verifies that streamed duplicate merging owns its mutable record rather than mutating shared cached events.
