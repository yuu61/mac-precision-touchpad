//! Running child processes with readable logging and errors.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

/// Render a command line for the log (quotes arguments that contain spaces).
///
/// The argument that follows any argument exactly equal (ASCII
/// case-insensitive) to one of `secret_flags` is shown as `<redacted>`.
/// Whole-argument match only: signtool's `/ph`, `/p7`, `/p7co` and `/p7ce`
/// are not secrets, only `/p <password>` is.
fn display(cmd: &Command, secret_flags: &[&str]) -> String {
    let mut s = quote(cmd.get_program());
    let mut hide_next = false;
    for a in cmd.get_args() {
        s.push(' ');
        if hide_next {
            s.push_str("<redacted>");
            hide_next = false;
        } else {
            s.push_str(&quote(a));
            let a = a.to_string_lossy();
            hide_next = secret_flags.iter().any(|f| a.eq_ignore_ascii_case(f));
        }
    }
    s
}

fn quote(s: &OsStr) -> String {
    let s = s.to_string_lossy();
    if s.is_empty() || s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.into_owned()
    }
}

/// Run `cmd` with inherited stdio; error unless it exits with status 0.
pub fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    run_redacted(cmd, what, &[])
}

/// `run`, but the value after any of `secret_flags` (e.g. signtool's
/// `/p <password>`) is shown as `<redacted>` in the log line and in the errors.
pub fn run_redacted(cmd: &mut Command, what: &str, secret_flags: &[&str]) -> Result<(), String> {
    let shown = display(cmd, secret_flags);
    let dir = cmd
        .get_current_dir()
        .map(|d| format!(" (in {})", d.display()))
        .unwrap_or_default();
    println!("xtask: > {shown}{dir}");
    let status = cmd
        .status()
        .map_err(|e| format!("{what}: could not start `{shown}`: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{what} failed ({}): `{shown}`",
            match status.code() {
                Some(c) => format!("exit code {c}"),
                None => "terminated".to_owned(),
            },
        ))
    }
}

/// Error unless `path` is an existing file.
pub fn require_file(path: &Path, hint: &str) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("{} not found{hint}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_only_the_value_after_a_secret_flag() {
        let mut cmd = Command::new("signtool");
        cmd.args([
            "sign", "/f", "a.pfx", "/P", "hunter2", "/p7", "x", "/ph", "/fd", "sha256", "f.sys",
        ]);
        let shown = display(&cmd, &["/p", "-p"]);
        assert!(!shown.contains("hunter2"), "{shown}");
        assert_eq!(
            shown,
            "signtool sign /f a.pfx /P <redacted> /p7 x /ph /fd sha256 f.sys"
        );
        let mut cmd = Command::new("signtool");
        cmd.args(["sign", "-p", "my secret", "f.sys"]);
        assert_eq!(
            display(&cmd, &["/p", "-p"]),
            "signtool sign -p <redacted> f.sys"
        );
        // Without secret flags nothing is hidden (and spaces are quoted).
        assert_eq!(display(&cmd, &[]), "signtool sign -p \"my secret\" f.sys");
    }
}
