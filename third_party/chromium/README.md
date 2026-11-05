# Chromium keycode table

`dom_code_data.inc` is copied unmodified from Chromium's
`ui/events/keycodes/dom/dom_code_data.inc` (main branch, commit
`3f9a34d7d1574001df41c7a10681691a2f04b8b4`, fetched 2026-10-21). It is
licensed under the BSD 3-Clause license in `LICENSE`.

`cargo xtask keymap` turns it into `crates/iw-keymap/src/table.rs`. To update,
replace the file with a newer copy and run the task again; the task refuses
tables in which two keys share a native code.
