# Project Notes

- Release builds use `panic = "abort"` workspace-wide; do not rely on `catch_unwind` boundaries in release behavior.
