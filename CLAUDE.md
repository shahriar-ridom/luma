# Luma

Linux file manager in Rust (GTK4 + libadwaita + GIO). App id `dev.ridom.Luma`, binary and packages `luma`, repo `shahriar-ridom/luma`.
Follow `SPEC.md` for product scope; `README.md` is user-facing and `docs/development.md` covers build, architecture and releases.

Key rules:
- GIO/GFile is the primary FS abstraction; `std::fs` only for direct local access.
- Never block the GTK main thread; UI updates only on the main thread. `core/` must not depend on widgets.
- Never silently overwrite files; no silent privilege escalation; commands run as argument lists, never shell strings.
- Icons are generated (`tools/gen_icons.py`, needs shapely/svgpathtools/numpy) and embedded in the binary. GTK ignores strokes in symbolic icons, so they are converted to outlines.
- Context menus are custom popovers (`ui/menu.rs`), not `PopoverMenu` (it caps height and scrolls).

Checks before committing: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `tools/check-version.sh`.
Release: `tools/bump-version.sh X.Y.Z`, edit CHANGELOG, tag `vX.Y.Z`.
Old data (`~/.local/share/file-manager/fm.sqlite`) is migrated to `~/.local/share/luma/luma.sqlite` on start.
