---
default: patch
---

# Fixed a memory error occurring during plugin execution

When the versions of crates providing types across the host/plugin dylib boundary did not match, it was not being detected, leading to errors such as `free(): invalid pointer`.  
Included fingerprints for crates that cross boundaries and ensured that the startup is rejected if a mismatch is detected.
