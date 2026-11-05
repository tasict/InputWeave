# Stubs for checking Windows code without Windows

`scripts/windows-check.sh` type-checks the workspace for a Windows target
on macOS or Linux, where there is no MSVC. ring's build script compiles a
little C; the headers in `include/` declare just what it uses, and
`fake-lib.sh` archives the objects with `ar` in place of `lib.exe`. Nothing
is linked or run, so passing the check says nothing about whether the code
works on Windows.
