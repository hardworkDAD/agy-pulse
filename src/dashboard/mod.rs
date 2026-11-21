mod http_wire;
mod server;

use std::env;
use std::io::{self, Write};

use crate::config::{self, LanguageSetting};
use crate::platform;
use crate::render::resolve_language_from_locale;

pub use server::{DashboardError, DashboardServer, DashboardServerConfig, DashboardServerInfo};

pub const DEFAULT_DASHBOARD_PORT: u16 = 3789;
pub const LOOPBACK_HOST: &str = "127.0.0.1";
pub const DASHBOARD_APP_ID: &str = "agy-pulse-dashboard";
pub const DEFAULT_IDLE_TIMEOUT_SEC: u64 = 600;

pub const DASHBOARD_ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64" fill="none">
  <!-- agy-pulse-dashboard-icon -->
  <rect x="2" y="2" width="60" height="60" rx="14" fill="#0b1120" stroke="#10b981" stroke-width="2.5"/>
  <polyline points="10 34 20 34 26 16 34 48 41 26 46 34 54 34" stroke="#10b981" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"/>
  <circle cx="54" cy="34" r="3" fill="#38bdf8"/>
</svg>"##;

const DASHBOARD_HTML_TEMPLATE: &str = include_str!("assets/dashboard.html");
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Subcommands supported by `agypl dashboard <action>` (launcher actions wired in Ticket 08).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DashboardAction {
    #[default]
    Serve,
    Open,
    Stop,
    Status,
    InstallShortcut,
    UninstallShortcut,
}

/// Parsed CLI flags for `agypl dashboard` / `agy-pulse --dashboard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardCliArgs {
    pub port: u16,
    pub open: bool,
    pub host: String,
    pub lang: LanguageSetting,
    pub no_sync: bool,
    pub action: DashboardAction,
    pub app_mode: bool,
    pub background_serve: bool,
    pub idle_timeout_sec: u64,
    pub include_desktop: bool,
    pub help: bool,
}

impl Default for DashboardCliArgs {
    fn default() -> Self {
        Self {
            port: DEFAULT_DASHBOARD_PORT,
            open: true,
            host: LOOPBACK_HOST.to_string(),
            lang: LanguageSetting::Auto,
            no_sync: false,
            action: DashboardAction::Serve,
            app_mode: false,
            background_serve: false,
            idle_timeout_sec: 0,
            include_desktop: false,
            help: false,
        }
    }
}

fn parse_action_alias(raw: &str) -> Option<DashboardAction> {
    match raw {
        "open" => Some(DashboardAction::Open),
        "stop" => Some(DashboardAction::Stop),
        "status" => Some(DashboardAction::Status),
        "install-shortcut" | "install-app" => Some(DashboardAction::InstallShortcut),
        "uninstall-shortcut" | "uninstall-app" => Some(DashboardAction::UninstallShortcut),
        _ => None,
    }
}

/// Parse CLI arguments for `agypl dashboard` / `dash` / `ui` / `--dashboard`.
pub fn parse_dashboard_args(argv: &[String], env_idle_sec: Option<&str>) -> DashboardCliArgs {
    let mut port = DEFAULT_DASHBOARD_PORT;
    let mut open = true;
    let mut host = LOOPBACK_HOST.to_string();
    let mut lang = LanguageSetting::Auto;
    let mut no_sync = false;
    let mut action = DashboardAction::Serve;
    let mut app_mode_explicit: Option<bool> = None;
    let mut background_serve = false;
    let mut idle_timeout_explicit: Option<u64> = None;
    let mut include_desktop = false;
    let mut help = false;

    let mut start_idx = 0usize;
    while start_idx < argv.len() && argv[start_idx].is_empty() {
        start_idx += 1;
    }
    if start_idx < argv.len()
        && matches!(
            argv[start_idx].as_str(),
            "dashboard" | "dash" | "ui" | "--dashboard"
        )
    {
        start_idx += 1;
    }
    while start_idx < argv.len() && argv[start_idx].is_empty() {
        start_idx += 1;
    }
    if start_idx < argv.len() {
        let maybe_action = argv[start_idx].as_str();
        if !maybe_action.starts_with('-')
            && let Some(parsed_action) = parse_action_alias(maybe_action)
        {
            action = parsed_action;
            start_idx += 1;
        }
    }

    let mut i = start_idx;
    while i < argv.len() {
        let raw = argv[i].as_str();
        i += 1;
        if raw.is_empty() || raw == "--dashboard" {
            continue;
        }

        let (flag, inline_val) = match raw.find('=') {
            Some(pos) if pos > 0 => (&raw[..pos], Some(&raw[pos + 1..])),
            _ => (raw, None),
        };

        let mut consume_value = || -> Option<String> {
            if let Some(v) = inline_val {
                return Some(v.to_string());
            }
            if i < argv.len() {
                let val = argv[i].clone();
                i += 1;
                return Some(val);
            }
            None
        };

        match flag {
            "--help" | "-h" => help = true,
            "--no-open" => open = false,
            "--open" => open = true,
            "--no-sync" => no_sync = true,
            "--zh" => lang = LanguageSetting::Zh,
            "--en" => lang = LanguageSetting::En,
            "--app" => app_mode_explicit = Some(true),
            "--browser" => app_mode_explicit = Some(false),
            "--background" | "--serve-background" => background_serve = true,
            "--desktop" => include_desktop = true,
            "--host" => {
                if let Some(val) = consume_value()
                    && !val.trim().is_empty()
                {
                    host = val.trim().to_string();
                }
            }
            "--idle-timeout" => {
                if let Some(val) = consume_value()
                    && let Ok(parsed) = val.trim().parse::<u64>()
                {
                    idle_timeout_explicit = Some(parsed);
                }
            }
            "--port" | "-p" => {
                if let Some(val) = consume_value()
                    && let Ok(parsed) = val.trim().parse::<u16>()
                {
                    port = parsed;
                }
            }
            "--lang" => {
                if let Some(val) = consume_value() {
                    match val.trim().to_ascii_lowercase().as_str() {
                        "zh" => lang = LanguageSetting::Zh,
                        "en" => lang = LanguageSetting::En,
                        "auto" => lang = LanguageSetting::Auto,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    let app_mode = app_mode_explicit.unwrap_or(action == DashboardAction::Open);
    let parsed_env_idle = env_idle_sec
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<u64>().ok());
    let default_idle_sec = if background_serve || action == DashboardAction::Open {
        parsed_env_idle.unwrap_or(DEFAULT_IDLE_TIMEOUT_SEC)
    } else {
        0
    };
    let idle_timeout_sec = idle_timeout_explicit.unwrap_or(default_idle_sec);

    DashboardCliArgs {
        port,
        open,
        host,
        lang,
        no_sync,
        action,
        app_mode,
        background_serve,
        idle_timeout_sec,
        include_desktop,
        help,
    }
}

/// Render the zero-CDN embedded HTML dashboard page with sanitized initial language and version.
pub fn render_dashboard_html(initial_lang: LanguageSetting) -> String {
    let (html_lang, js_lang) = match initial_lang {
        LanguageSetting::Zh => ("zh-CN", "zh"),
        LanguageSetting::En | LanguageSetting::Auto => ("en", "en"),
    };
    DASHBOARD_HTML_TEMPLATE
        .replace("__AGY_PULSE_HTML_LANG__", html_lang)
        .replace("__AGY_PULSE_INITIAL_LANG__", js_lang)
        .replace("__AGY_PULSE_VERSION__", VERSION)
}

/// Localized help text for `agypl dashboard --help`.
pub fn build_dashboard_help_text(lang: LanguageSetting) -> String {
    if lang == LanguageSetting::Zh {
        format!(
            "agy-pulse v{VERSION} — 本地回环网页看板\n\n\
用法:\n  \
  agypl dashboard [options]               启动本地 127.0.0.1 网页看板（别名: dash, ui, --dashboard）\n\n\
选项:\n  \
  --port, -p <port>                       监听端口（默认 3789，冲突自动回退随机端口）\n  \
  --no-open                               仅启动服务，不自动打开浏览器\n  \
  --no-sync                               跳过启动及查询时的自动账本扫描\n  \
  --zh / --en / --lang <auto|zh|en>       指定界面语言\n  \
  --idle-timeout <sec>                    无请求自动退出秒数（默认 0 常驻）\n  \
  --help, -h                              显示此帮助信息\n"
        )
    } else {
        format!(
            "agy-pulse v{VERSION} — Local Loopback Telemetry Dashboard\n\n\
Usage:\n  \
  agypl dashboard [options]               Start local 127.0.0.1 web dashboard (alias: dash, ui, --dashboard)\n\n\
Options:\n  \
  --port, -p <port>                       Listen port (default 3789, auto-fallback on collision)\n  \
  --no-open                               Do not open browser automatically\n  \
  --no-sync                               Skip automatic ledger sync on startup/queries\n  \
  --zh / --en / --lang <auto|zh|en>       Set initial UI language\n  \
  --idle-timeout <sec>                    Idle auto-exit timeout in seconds (default 0)\n  \
  --help, -h                              Show this help message\n"
        )
    }
}

/// Execute `agypl dashboard` / `dash` / `ui` / `--dashboard` CLI entrypoint.
pub fn run_dashboard_cli(argv: &[String]) -> i32 {
    let env_idle = env::var("AGY_PULSE_DASHBOARD_IDLE_SEC").ok();
    let args = parse_dashboard_args(argv, env_idle.as_deref());
    let cwd = env::current_dir().unwrap_or_default();

    let effective_lang = if args.lang == LanguageSetting::Auto {
        config::load_config(&cwd).language
    } else {
        args.lang
    };
    let locale = platform::read_locale_env();
    let resolved_lang = resolve_language_from_locale(effective_lang, locale.as_deref());
    let is_zh = resolved_lang == LanguageSetting::Zh;

    if args.help {
        print!("{}", build_dashboard_help_text(resolved_lang));
        return 0;
    }

    if args.action != DashboardAction::Serve {
        eprintln!(
            "agy-pulse dashboard launcher subcommand '{:?}' requires Ticket 08 daemon/shortcut support",
            args.action
        );
        return 1;
    }

    let cfg = DashboardServerConfig {
        host: args.host,
        port: args.port,
        lang: resolved_lang,
        no_sync: args.no_sync,
        defer_initial_sync: false,
        idle_timeout_ms: args.idle_timeout_sec.saturating_mul(1000),
        ..Default::default()
    };

    let mut server = match DashboardServer::start(cfg) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("agy-pulse dashboard error: {err}");
            return 1;
        }
    };

    let info = server.info().clone();
    if !args.no_sync
        && let Some(ref report) = info.sync_report
    {
        if is_zh {
            println!(
                "agy-pulse 遥测账本已同步: 扫描 {} 个库，归档 {} 步 ({}ms)",
                report.total_dbs, report.steps_upserted, report.duration_ms
            );
        } else {
            println!(
                "agy-pulse ledger synced: {} DBs scanned, {} steps upserted ({}ms)",
                report.total_dbs, report.steps_upserted, report.duration_ms
            );
        }
    }

    if is_zh {
        println!(
            "agy-pulse 本地看板已启动: {}（按 Ctrl+C 退出）",
            info.url
        );
    } else {
        println!(
            "agy-pulse dashboard listening at {} (press Ctrl+C to stop)",
            info.url
        );
    }
    let _ = io::stdout().flush();

    server.wait_until_stopped();
    0
}
