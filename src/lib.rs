#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod config;
pub mod domain;
pub mod ingest;
pub mod platform;
pub mod render;

use std::env;
use std::io::{self, Read};
use std::panic;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::ingest::HostReader;
use crate::render::resolve_language_from_locale;

const STDIN_HARD_TIMEOUT: Duration = Duration::from_millis(1500);
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn build_cli_help_text() -> String {
    format!(
        "agy-pulse v{VERSION} — Antigravity CLI status line & telemetry (CLI: agypl / agy-pulse)\n\n\
Usage:\n  \
  agypl dashboard [options]               Start local web dashboard (alias: dash, ui, --dashboard)\n  \
  agypl stats [today|7d|30d|all]          Show CLI & IDE telemetry summary (alias: report, --stats)\n  \
  agypl config                            Launch interactive configuration wizard (alias: --config)\n  \
  agypl update                            Check and install the latest release (alias: --update)\n  \
  agypl help                              Show this help message (alias: --help, -h)\n  \
  agypl --version                         Show version information (alias: -V, -v, version)\n  \
  agy-pulse                               Render status line from stdin JSON (or use any subcommand above)\n"
    )
}

fn read_stdin_with_timeout(timeout: Duration) -> (String, bool) {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut timed_out = false;

    loop {
        let now = Instant::now();
        if now >= deadline {
            timed_out = true;
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(chunk) => bytes.extend_from_slice(&chunk),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timed_out = true;
                break;
            }
        }
    }

    (String::from_utf8_lossy(&bytes).into_owned(), timed_out)
}

fn run_statusline_pipeline() {
    let (stdin_text, timed_out) = read_stdin_with_timeout(STDIN_HARD_TIMEOUT);
    if timed_out && stdin_text.trim().is_empty() {
        platform::write_error_log_safely("Statusline stdin read timed out after 1500ms");
    }

    let fallback_cwd = env::current_dir().unwrap_or_default();
    let ingest = HostReader::read_live_pulse(&stdin_text, &fallback_cwd);
    if !ingest.diagnostics.is_empty() {
        platform::write_error_log_safely(&ingest.diagnostics.join("\n"));
    }

    let cfg = config::load_config(&ingest.workspace_dir);
    if !cfg.enabled {
        return;
    }

    let term = platform::detect_terminal_caps(
        ingest.snapshot.terminal_width,
        cfg.display.terminal_width,
        cfg.display.unicode,
    );
    let locale = platform::read_locale_env();
    let resolved_lang = resolve_language_from_locale(cfg.language, locale.as_deref());
    let output = render::render_statusline(&ingest.snapshot, &cfg, &term, resolved_lang);
    if !output.is_empty() {
        print!("{output}");
    }
}

/// Primary CLI entrypoint shared by `agy-pulse` and `agypl` binaries.
pub fn run_cli() -> i32 {
    let args: Vec<String> = env::args().skip(1).collect();
    let first = args.first().map(String::as_str);

    if matches!(first, Some("--version" | "-V" | "-v" | "version"))
        || args.iter().any(|a| a == "--version" || a == "-V")
    {
        println!("agy-pulse v{VERSION}");
        return 0;
    }

    if matches!(first, Some("help" | "--help" | "-h"))
        || args.iter().any(|a| a == "--help" || a == "-h")
    {
        print!("{}", build_cli_help_text());
        return 0;
    }

    let res = panic::catch_unwind(run_statusline_pipeline);
    if res.is_err() {
        platform::write_error_log_safely("Recovered from unexpected panic in statusline mode");
    }
    0
}
