//! Building Windows command lines, free of FFI so it is tested on every
//! OS.

#![cfg_attr(not(windows), allow(dead_code))]

/// The command line that runs `program` with `args`, for
/// `CreateProcessAsUserW`.
///
/// The program path is always quoted. Unquoted, a path with spaces such as
/// `C:\Program Files\InputWeave\inputweave.exe` would make Windows try
/// `C:\Program.exe` first, which for a service is a privilege escalation.
/// Paths cannot contain quotes, so plain quoting is exact for it.
pub(crate) fn command_line(program: &str, args: &[&str]) -> String {
    let mut line = format!("\"{program}\"");
    for arg in args {
        line.push(' ');
        line.push_str(&quote(arg));
    }
    line
}

/// `arg` quoted so that `CommandLineToArgvW` and the C runtime read it
/// back unchanged: backslashes are literal except before a quote, where
/// they and the quote are escaped.
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // Before the closing quote, every backslash needs escaping.
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits the arguments after the program name the way
    /// `CommandLineToArgvW` documents it.
    fn split(line: &str) -> Vec<String> {
        let mut args = Vec::new();
        let mut chars = line.chars().peekable();
        loop {
            while chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
                chars.next();
            }
            if chars.peek().is_none() {
                return args;
            }
            let mut arg = String::new();
            let mut quoted = false;
            while let Some(&c) = chars.peek() {
                if !quoted && (c == ' ' || c == '\t') {
                    break;
                }
                chars.next();
                match c {
                    '\\' => {
                        let mut n = 1;
                        while chars.peek() == Some(&'\\') {
                            chars.next();
                            n += 1;
                        }
                        if chars.peek() == Some(&'"') {
                            arg.push_str(&"\\".repeat(n / 2));
                            if n % 2 == 1 {
                                chars.next();
                                arg.push('"');
                            }
                        } else {
                            arg.push_str(&"\\".repeat(n));
                        }
                    }
                    '"' => quoted = !quoted,
                    _ => arg.push(c),
                }
            }
            args.push(arg);
        }
    }

    #[test]
    fn the_program_is_always_quoted() {
        assert_eq!(
            command_line(r"C:\Program Files\InputWeave\inputweave.exe", &["agent"]),
            r#""C:\Program Files\InputWeave\inputweave.exe" agent"#
        );
        assert_eq!(command_line(r"C:\iw.exe", &[]), r#""C:\iw.exe""#);
    }

    #[test]
    fn arguments_read_back_unchanged() {
        let tricky = [
            "broker",
            "",
            "two words",
            r#"say "hi""#,
            r"C:\dir\",
            r"C:\some dir\",
            r#"back\\"slash"#,
            "tab\there",
            r"\\server\share",
        ];
        let line = command_line(r"C:\iw.exe", &tricky);
        let rest = line.strip_prefix(r#""C:\iw.exe""#).unwrap();
        assert_eq!(split(rest), tricky);
        assert_eq!(quote("agent"), "agent");
        assert_eq!(quote(r"a b\"), r#""a b\\""#);
    }
}
