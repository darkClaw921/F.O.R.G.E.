# agents::store::tests::cached_owner_availability_preserves_parent_only_and_healthy_duplicate_semantics

Regression for the optimized idle path. A failed child-only source marks the child unavailable but never marks a parent-only root unavailable; adding an unchanged healthy duplicate removes that unavailable status. Sources are in-memory old metadata and no original journal or user cache is accessed. This protects behavior that a naive union of related session_ids would change.
