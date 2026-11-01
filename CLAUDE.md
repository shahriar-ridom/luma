# File Manager

Linux file manager in Rust (GTK4 + Libadwaita + GIO). **Follow `SPEC.md`** — it is the source of truth for stack, architecture (§8), roadmap (§12) and MVP scope (§13).

Key rules from the spec:
- GIO/GFile is the primary FS abstraction; `std::fs` only for direct local access.
- Never block the GTK main thread; UI updates only on main thread.
- Core (filesystem/navigation/operations) must not depend on UI.
- Build in roadmap order; MVP first, no advanced features before a working file manager.
- Never silently overwrite files; no silent privilege escalation.

Commands: `cargo build`, `cargo run`, `cargo test`.
