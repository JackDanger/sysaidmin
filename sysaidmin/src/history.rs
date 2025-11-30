use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Writes command history to sysaidmin.history.sh in bash script format.
/// Each command is written with STDOUT and STDERR captured in comments.
pub struct HistoryWriter {
    file: Arc<Mutex<File>>,
    #[allow(dead_code)]
    path: PathBuf,
}

impl HistoryWriter {
    pub fn new(history_path: PathBuf) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&history_path)?;

        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            path: history_path,
        })
    }

    /// Append a command to the history file.
    /// The command will be written as-is, followed by comments for stdout/stderr.
    pub fn append_command(
        &self,
        command: &str,
        cwd: Option<&str>,
        stdout: &str,
        stderr: &str,
    ) -> std::io::Result<()> {
        let mut file = self.file.lock().map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to lock history file: {}", e),
            )
        })?;

        // Write CWD change if needed
        if let Some(cwd) = cwd {
            writeln!(file, "cd {}", escape_shell_arg(cwd))?;
        }

        // Write the command
        writeln!(file, "{}", command)?;

        // Write stdout as comment if present
        if !stdout.trim().is_empty() {
            for line in stdout.lines() {
                writeln!(file, "#> {}", escape_comment(line))?;
            }
        }

        // Write stderr as comment if present
        if !stderr.trim().is_empty() {
            for line in stderr.lines() {
                writeln!(file, "#err: {}", escape_comment(line))?;
            }
        }

        // Add blank line for readability
        writeln!(file)?;
        file.flush()?;
        Ok(())
    }
}

/// Escape a shell argument for safe use in bash
fn escape_shell_arg(arg: &str) -> String {
    // Simple escaping: wrap in single quotes and escape single quotes
    format!("'{}'", arg.replace('\'', "'\"'\"'"))
}

/// Escape a comment line (remove newlines, handle special chars)
fn escape_comment(line: &str) -> String {
    line.replace('\n', " ").replace('\r', "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn writes_command_with_output() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("history.sh");
        let writer = HistoryWriter::new(path.clone()).unwrap();

        writer
            .append_command(
                "echo hello",
                None,
                "hello\n",
                "",
            )
            .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("echo hello"));
        assert!(content.contains("#> hello"));
    }

    #[test]
    fn writes_command_with_cwd() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("history.sh");
        let writer = HistoryWriter::new(path.clone()).unwrap();

        writer
            .append_command(
                "ls",
                Some("/tmp"),
                "",
                "",
            )
            .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("cd '/tmp'"));
        assert!(content.contains("ls"));
    }

    #[test]
    fn writes_stderr_comments() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("history.sh");
        let writer = HistoryWriter::new(path.clone()).unwrap();

        writer
            .append_command(
                "ls /nonexistent",
                None,
                "",
                "ls: /nonexistent: No such file or directory",
            )
            .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("#err: ls: /nonexistent: No such file or directory"));
    }

    #[test]
    fn escapes_special_chars_in_cwd() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("history.sh");
        let writer = HistoryWriter::new(path.clone()).unwrap();

        writer
            .append_command(
                "echo test",
                Some("/path/with'single'quotes"),
                "",
                "",
            )
            .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("cd '/path/with'\"'\"'single'\"'\"'quotes'"));
    }
}

