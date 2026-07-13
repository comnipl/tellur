---
default: patch
---

# Fixed VRAM exhaustion panics and preview server lock poisoning

Long Tellur Live sessions could crash with `render context could not read back GPU image ...` and then permanently fail every request with `preview app lock poisoned` until restart. Two global composite caches pinned GPU memory with no byte bound and no way to release it, so the emergency eviction that runs before a readback could not actually free VRAM.

The composite caches are now bounded by bytes (1/16 of the VRAM budget, capped at 256 MiB each) in addition to the entry cap, and are cleared on plugin reload and as a last resort when a VRAM reservation fails. Tellur Live also no longer poisons the preview lock when a render panics: the panic is caught and returned as an HTTP error, and the server keeps serving subsequent requests.
