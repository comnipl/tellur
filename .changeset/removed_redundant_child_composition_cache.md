---
default: patch
---

# Removed the redundant child-composition cache

The cache only skipped the final blend after independently cached child images had already been rendered, while retaining both its inputs and output outside the main component cache. That limited reuse did not justify its memory and lifecycle cost, so child batches are now recomposited when needed.
