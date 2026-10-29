# AttentionState::set_generating

Legacy test-only helper for boolean-front generating/idle regression tests. It is compiled only under cfg(test); production watcher never calls it and never uses pane hashes to drive generation. The production writer is AttentionState::apply_agent_activity with coherent journal-derived original start/count/providers and explicit-ended-only idle approval. Existing tests of historical set_generating behavior remain isolated to prevent accidental changes in attached/needs_attention idle semantics.
