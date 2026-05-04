//! Per-shell argument quoting + argv construction.
//!
//! Each Windows shell has its own escape syntax. These helpers harden
//! `shell_exec` against argv-injection attacks by encoding user-supplied
//! arguments inside the chosen shell's quoting rules.
//!
//! Examples:
//! ```
//! use fastuse_core::shell::quoting::{quote, build_argv, Shell};
//! assert_eq!(quote(Shell::Cmd, "hello world"), "\"hello world\"");
//! assert_eq!(quote(Shell::Powershell, "it's"), "'it''s'");
//! assert_eq!(build_argv(Shell::Bash, "ls"), vec!["bash.exe".to_string(), "-c".into(), "ls".into()]);
//! ```

use serde::{Deserialize, Serialize};

/// Shell choice for `shell_exec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Shell {
    /// `cmd.exe` (default on Windows).
    Cmd,
    /// `powershell.exe` (Windows PowerShell 5.1).
    Powershell,
    /// `pwsh.exe` (PowerShell 7+).
    Pwsh,
    /// `bash.exe` (Git Bash, WSL, MSYS).
    Bash,
}

/// Quote a single argument for the given shell.
///
/// - **cmd**: wrap in `"..."`, double internal `"` to `""`, caret-escape
///   `& | < > ^` inside the quotes.
/// - **powershell / pwsh**: single-quote, double internal `'`. No expansion.
/// - **bash**: single-quote, close-escape-reopen for embedded `'`.
pub fn quote(shell: Shell, arg: &str) -> String {
    match shell {
        Shell::Cmd => quote_cmd(arg),
        Shell::Powershell | Shell::Pwsh => quote_powershell(arg),
        Shell::Bash => quote_bash(arg),
    }
}

fn quote_cmd(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        match c {
            '"' => out.push_str("\"\""),
            '&' | '|' | '<' | '>' | '^' => {
                out.push('^');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn quote_powershell(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for c in arg.chars() {
        if c == '\'' {
            out.push_str("''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn quote_bash(arg: &str) -> String {
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for c in arg.chars() {
        if c == '\'' {
            // Close, escape, reopen: 'it'\''s'
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Build the full argv (program + flags + command) for `tokio::process::Command`.
///
/// The returned `Vec<String>`'s first entry is the executable name and the
/// remainder are flags + the user's `command` passed verbatim. The caller
/// should pass the user's command as a single string (the shell does its own
/// internal parsing).
pub fn build_argv(shell: Shell, command: &str) -> Vec<String> {
    match shell {
        Shell::Cmd => vec![
            "cmd.exe".into(),
            "/D".into(),
            "/C".into(),
            command.to_string(),
        ],
        Shell::Powershell => vec![
            "powershell.exe".into(),
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            command.to_string(),
        ],
        Shell::Pwsh => vec![
            "pwsh.exe".into(),
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            command.to_string(),
        ],
        Shell::Bash => vec!["bash.exe".into(), "-c".into(), command.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_quotes_space() {
        assert_eq!(quote(Shell::Cmd, "hello world"), "\"hello world\"");
    }

    #[test]
    fn cmd_caret_escapes_metachars() {
        assert_eq!(quote(Shell::Cmd, "a&b"), "\"a^&b\"");
        assert_eq!(quote(Shell::Cmd, "a|b"), "\"a^|b\"");
        assert_eq!(quote(Shell::Cmd, "a<b"), "\"a^<b\"");
    }

    #[test]
    fn cmd_doubles_internal_quote() {
        assert_eq!(quote(Shell::Cmd, "a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn powershell_doubles_single_quote() {
        assert_eq!(quote(Shell::Powershell, "it's"), "'it''s'");
    }

    #[test]
    fn powershell_no_expansion_in_single_quotes() {
        assert_eq!(quote(Shell::Pwsh, "$x"), "'$x'");
    }

    #[test]
    fn bash_close_escape_reopen() {
        assert_eq!(quote(Shell::Bash, "it's"), "'it'\\''s'");
    }

    #[test]
    fn build_argv_cmd() {
        assert_eq!(
            build_argv(Shell::Cmd, "echo hi & dir"),
            vec!["cmd.exe", "/D", "/C", "echo hi & dir"]
        );
    }

    #[test]
    fn build_argv_powershell() {
        assert_eq!(
            build_argv(Shell::Powershell, "Get-ChildItem"),
            vec![
                "powershell.exe",
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-ChildItem"
            ]
        );
    }

    #[test]
    fn build_argv_pwsh() {
        assert_eq!(
            build_argv(Shell::Pwsh, "Get-Date")[0],
            "pwsh.exe"
        );
    }

    #[test]
    fn build_argv_bash() {
        assert_eq!(
            build_argv(Shell::Bash, "ls"),
            vec!["bash.exe", "-c", "ls"]
        );
    }
}
