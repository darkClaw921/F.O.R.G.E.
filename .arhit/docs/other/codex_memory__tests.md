# codex_memory::tests

Six isolated fixture tests cover native global index plus summary/rollout and sub-agent memory read/write, UTF-8 and DTO keys, absent store versus absent native index, traversal/source/suffix validation and no file creation, symlink file/parent/source rejection without touching external target, read truncation and write size rejection, plus HTTP remote/invalid JSON contracts. All filesystem mutation occurs under TempDir; tests never set HOME/CODEX_HOME or write user memory.
