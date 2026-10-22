//! `dup-detector doctor`: health checks for the grammar-plugin toolchain.
//!
//! Verifies everything `dup-detector install` relies on (git, a working C
//! compiler, a writable grammars directory) and re-validates every installed
//! plugin by loading it into a scratch registry. Exits non-zero when any
//! check fails so it can gate scripts.

use std::io;
use std::process::Command;

use crate::install;
use crate::plugin;

const PROBE_SOURCE: &str = "int dup_detector_probe(void) { return 0; }\n";

/// Run all checks, printing a report. Returns `true` when everything is
/// healthy.
pub fn run() -> bool {
    let mut healthy = true;
    println!("plugin toolchain:");
    healthy &= check_program("git", &["--version"], "downloads grammar repositories");
    let compiler = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    healthy &= check_compiler(&compiler);
    healthy &= check_grammars_dir();
    println!("installed plugins:");
    healthy &= check_plugins();
    if healthy {
        println!("doctor: all checks passed");
    } else {
        println!("doctor: problems found (see above)");
    }
    healthy
}

fn check_program(name: &str, args: &[&str], purpose: &str) -> bool {
    match Command::new(name).args(args).output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            println!(
                "  {name:<12} ok    {}",
                version.trim().lines().next().unwrap_or("")
            );
            true
        }
        Ok(_) => {
            println!("  {name:<12} ERROR runs but exits non-zero ({purpose})");
            false
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            println!("  {name:<12} MISSING needed to {purpose}");
            false
        }
        Err(error) => {
            println!("  {name:<12} ERROR cannot execute: {error}");
            false
        }
    }
}

/// The compiler must not only exist but actually produce a shared library,
/// so probe it with a real compile using the same flags as `install`.
fn check_compiler(compiler: &str) -> bool {
    if !cfg!(unix) {
        println!(
            "  {:<12} ERROR building grammar libraries is not supported on this platform",
            "cc"
        );
        return false;
    }
    let probe_dir = std::env::temp_dir().join(format!(
        "dup-detector-doctor-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("main")
    ));
    let _ = std::fs::remove_dir_all(&probe_dir);
    let result = (|| -> Result<(), String> {
        std::fs::create_dir_all(&probe_dir).map_err(|e| e.to_string())?;
        let source = probe_dir.join("probe.c");
        std::fs::write(&source, PROBE_SOURCE).map_err(|e| e.to_string())?;
        install::compile_shared_library(&[source], &probe_dir.join("probe.so"))
            .map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_dir_all(&probe_dir);
    match result {
        Ok(()) => {
            println!("  {:<12} ok    {compiler} (test compile passed)", "cc");
            true
        }
        Err(message) => {
            println!(
                "  {:<12} ERROR cannot build a shared library with {compiler}: {message}",
                "cc"
            );
            false
        }
    }
}

fn check_grammars_dir() -> bool {
    let Some(dir) = plugin::user_grammars_dir() else {
        println!("  grammars dir ERROR cannot resolve the user config directory");
        return false;
    };
    let probe = dir.join(".doctor-write-probe");
    match std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&probe, b"")) {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            println!("  {:<12} ok    {} (writable)", "grammars", dir.display());
            true
        }
        Err(error) => {
            println!(
                "  {:<12} ERROR {} is not writable: {error}",
                "grammars",
                dir.display()
            );
            false
        }
    }
}

fn check_plugins() -> bool {
    let Some(dir) = plugin::user_grammars_dir() else {
        return true;
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            println!("  (none installed)");
            return true;
        }
        Err(error) => {
            println!("  ERROR cannot list {}: {error}", dir.display());
            return false;
        }
    };
    let mut healthy = true;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join(plugin::MANIFEST_FILE_NAME).is_file() {
            continue;
        }
        match plugin::check_plugin(&path) {
            Ok(health) => {
                let extensions = health
                    .extensions
                    .iter()
                    .map(|ext| format!(".{ext}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!(
                    "  {} ok    {} ({}) abi {}",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                    health.name,
                    extensions,
                    health.abi_version,
                );
            }
            Err(error) => {
                println!(
                    "  {} ERROR {error}",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                );
                healthy = false;
            }
        }
    }
    healthy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_program_reports_failure() {
        assert!(!check_program(
            "dup-detector-definitely-missing-program",
            &["--version"],
            "test"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn compiler_probe_compiles() {
        assert!(check_compiler(
            &std::env::var("CC").unwrap_or_else(|_| "cc".to_string())
        ));
    }
}
