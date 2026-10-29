# codex_memory::put_memory

PUT /api/codex-memory handler rejects remote requests from query or body, resolves the local CLI home and runs save_into on Tokio blocking workers. Success returns 204; invalid paths return 400, absent home/file 404, oversized content 413, filesystem/task failure 500. No Codex process, model, memory generation or automatic approval is invoked.
