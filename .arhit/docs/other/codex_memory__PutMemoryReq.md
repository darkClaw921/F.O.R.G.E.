# codex_memory::PutMemoryReq

PUT request DTO containing root-relative file, UTF-8 content and optional server. Unknown path fields from the shared frontend are safely ignored: project paths do not select a Codex memory store. A nonempty remote server field is rejected. Files must already exist and pass source/path/size validation.
