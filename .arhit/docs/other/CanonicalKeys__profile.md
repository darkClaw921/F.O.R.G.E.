# CanonicalKeys::profile

Returns the original canonical provider/profile JSON tuple used for global usage deduplication. A nested per-provider profile map encodes once per distinct profile and uses borrowed lookups on subsequent events. This avoids repeated profile-path allocation and JSON serialization while retaining the exact native-response and cumulative-suppression domain.
