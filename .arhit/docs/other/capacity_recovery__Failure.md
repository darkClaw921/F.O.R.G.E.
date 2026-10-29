# capacity_recovery::Failure

Metadata-only observation carrying a64-bit hash of normalized preceding conversation/error context plus terminal dimensions. Footer clocks, colors, cursor values and wrap whitespace are excluded from the hash. Dimensions are kept separately so resizing and exposed history cannot be mistaken for a new error.
