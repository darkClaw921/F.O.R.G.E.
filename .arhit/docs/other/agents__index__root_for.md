# agents::index::root_for

Resolves a canonical session through the parent map and keeps the existing visited-set cycle guard. Root sessions absent from the parent map return immediately with one owned key, avoiding a temporary visited tree for every root event. Child and cyclic parent behavior remains unchanged; this is an allocation fast path, not a change to root attribution.
