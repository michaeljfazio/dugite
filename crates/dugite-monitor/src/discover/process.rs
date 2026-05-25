//! Process enumeration via `sysinfo`. Finds dugite-node PIDs and
//! extracts the `--database-path` argument from each command line.

use std::path::PathBuf;

use sysinfo::{ProcessRefreshKind, RefreshKind, System};

/// Information about a single discovered dugite-node process.
#[derive(Debug, Clone)]
pub(super) struct DugiteProcess {
    pub pid: u32,
    pub db_path: Option<PathBuf>,
}

/// Find every running process whose executable name is exactly
/// `dugite-node`, returning its PID and (if parseable) the
/// `--database-path` argument.
pub(super) fn find_dugite_node_processes() -> Vec<DugiteProcess> {
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);

    let mut out = Vec::new();
    for (pid, proc_) in sys.processes() {
        let name = proc_.name().to_string_lossy();
        if name != "dugite-node" {
            continue;
        }
        let cmdline: Vec<String> = proc_
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        out.push(DugiteProcess {
            pid: pid.as_u32(),
            db_path: extract_db_path_from_cmdline(&cmdline),
        });
    }
    out
}

/// Extract the value of `--database-path` (or its alias `--db-path`)
/// from a command line argv. Supports `--database-path X` and
/// `--database-path=X`. First occurrence wins.
pub(super) fn extract_db_path_from_cmdline(cmdline: &[String]) -> Option<PathBuf> {
    let mut iter = cmdline.iter();
    while let Some(arg) = iter.next() {
        for prefix in ["--database-path", "--db-path"] {
            if arg == prefix {
                return iter.next().map(PathBuf::from);
            }
            if let Some(rest) = arg.strip_prefix(&format!("{prefix}=")) {
                return Some(PathBuf::from(rest));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn extract_db_path_whitespace_form() {
        let cmd = argv(&[
            "dugite-node",
            "run",
            "--database-path",
            "/var/db",
            "--port",
            "3001",
        ]);
        assert_eq!(
            extract_db_path_from_cmdline(&cmd),
            Some(PathBuf::from("/var/db"))
        );
    }

    #[test]
    fn extract_db_path_equals_form() {
        let cmd = argv(&[
            "dugite-node",
            "run",
            "--database-path=/var/db",
            "--port",
            "3001",
        ]);
        assert_eq!(
            extract_db_path_from_cmdline(&cmd),
            Some(PathBuf::from("/var/db"))
        );
    }

    #[test]
    fn extract_db_path_alias_form() {
        let cmd = argv(&["dugite-node", "run", "--db-path", "./db-preview"]);
        assert_eq!(
            extract_db_path_from_cmdline(&cmd),
            Some(PathBuf::from("./db-preview"))
        );
    }

    #[test]
    fn extract_db_path_alias_equals_form() {
        let cmd = argv(&["dugite-node", "run", "--db-path=/var/db2"]);
        assert_eq!(
            extract_db_path_from_cmdline(&cmd),
            Some(PathBuf::from("/var/db2"))
        );
    }

    #[test]
    fn extract_db_path_missing_returns_none() {
        let cmd = argv(&["dugite-node", "run", "--port", "3001"]);
        assert_eq!(extract_db_path_from_cmdline(&cmd), None);
    }

    #[test]
    fn extract_db_path_flag_with_no_value_returns_none() {
        let cmd = argv(&["dugite-node", "run", "--database-path"]);
        assert_eq!(extract_db_path_from_cmdline(&cmd), None);
    }

    #[test]
    fn extract_db_path_first_occurrence_wins() {
        let cmd = argv(&[
            "dugite-node",
            "run",
            "--database-path",
            "/first",
            "--database-path",
            "/second",
        ]);
        assert_eq!(
            extract_db_path_from_cmdline(&cmd),
            Some(PathBuf::from("/first"))
        );
    }

    #[test]
    fn extract_db_path_empty_argv() {
        let cmd: Vec<String> = vec![];
        assert_eq!(extract_db_path_from_cmdline(&cmd), None);
    }
}
