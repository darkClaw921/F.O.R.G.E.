# agents::cache::tests::old_v3_cache_rebuilds_owner_ids_without_treating_parent_links_as_owners

Regression test for derived availability-cache compatibility. Constructs a child source linked to a root, verifies related IDs contain both but actual owners contain only the child, checks transient owner IDs never serialize, and compares borrowed cache save bytes against the previous owned v3 representation. Reloading that old-compatible snapshot must regenerate the transient owner cache without a version bump or history reset.
