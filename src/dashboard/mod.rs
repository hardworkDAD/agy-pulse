mod http_wire;
mod keepalive;
mod launcher;
mod server;

use std::env;
use std::io::{self, Write};

use crate::config::{self, LanguageSetting};
use crate::platform;
use crate::render::resolve_language_from_locale;

pub use launcher::{
    BrowserLaunchSpec, BrowserOpenResult, DASHBOARD_ICON_MARKER, DASHBOARD_PING_THROTTLE_MS,
    DASHBOARD_SHORTCUT_MARKER, DASHBOARD_SPAWN_LOCK_FILENAME, DASHBOARD_STATE_FILENAME,
    DashboardStateFile, EnsureDashboardResult, RunningDashboardInfo, SPAWN_LOCK_TTL_MS,
    STATUSLINE_IDLE_TIMEOUT_SEC, ShortcutInstallReport, ShortcutStatus, ShortcutUninstallReport,
    StatusLineDashboardInfo, StopDashboardResult, TouchDashboardResult,
    acquire_dashboard_spawn_lock, build_app_window_launch_spec, clear_dashboard_state_if_owner,
    dashboard_spawn_lock_path, dashboard_state_path, get_running_dashboard_info,
    is_valid_loopback_dashboard_url, ping_dashboard, probe_dashboard_health, read_dashboard_state,
    release_dashboard_spawn_lock, resolve_dashboard_port, resolve_dashboard_url_for_statusline,
    resolve_statusline_idle_timeout_sec, spawn_background_dashboard_detached,
    stop_background_dashboard, touch_running_dashboard, uninstall_desktop_shortcuts,
    write_dashboard_state,
};
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
    render_dashboard_html_with_token(initial_lang, "")
}

/// Render the zero-CDN embedded HTML dashboard page with sanitized language, version, and CSRF token.
pub fn render_dashboard_html_with_token(
    initial_lang: LanguageSetting,
    csrf_token: &str,
) -> String {
    let (html_lang, js_lang) = match initial_lang {
        LanguageSetting::Zh => ("zh-CN", "zh"),
        LanguageSetting::En | LanguageSetting::Auto => ("en", "en"),
    };
    DASHBOARD_HTML_TEMPLATE
        .replace("__AGY_PULSE_HTML_LANG__", html_lang)
        .replace("__AGY_PULSE_INITIAL_LANG__", js_lang)
        .replace("__AGY_PULSE_VERSION__", VERSION)
        .replace("__AGY_PULSE_CSRF_TOKEN__", csrf_token)
}

/// Localized help text for `agypl dashboard --help`.
pub fn build_dashboard_help_text(lang: LanguageSetting) -> String {
    if lang == LanguageSetting::Zh {
        format!(
            "agy-pulse v{VERSION} — 本地回环网页看板\n\n\
用法:\n  \
  agypl dashboard [action] [options]      启动或管理本地 127.0.0.1 网页看板（别名: dash, ui, --dashboard）\n\n\
子动作:\n  \
  open                                    在后台守护启动/复用看板，并以独立小窗打开\n  \
  status                                  查看后台看板与桌面快捷图标运行/安装状态\n  \
  stop                                    优雅停止后台运行中的看板守护进程\n  \
  install-shortcut                        安装系统应用菜单/桌面快捷图标（别名: install-app）\n  \
  uninstall-shortcut                      按专属指纹安全移除快捷图标（别名: uninstall-app）\n\n\
选项:\n  \
  --port, -p <port>                       监听端口（默认 3789，冲突自动回退随机端口）\n  \
  --host <addr>                           绑定地址（仅允许 127.0.0.1）\n  \
  --app / --browser                       优先以无地址栏独立小窗 / 默认浏览器标签页打开\n  \
  --desktop                               同时在桌面目录创建/清理快捷图标\n  \
  --no-open                               仅启动服务，不自动打开浏览器\n  \
  --no-sync                               跳过启动及查询时的自动账本扫描\n  \
  --zh / --en / --lang <auto|zh|en>       指定界面语言\n  \
  --idle-timeout <sec>                    无活跃页面连接时的自动退出秒数（后台默认 600，前台默认 0）\n  \
  --background                            作为后台守护模式运行（配合 open 使用）\n  \
  --help, -h                              显示此帮助信息\n"
        )
    } else {
        format!(
            "agy-pulse v{VERSION} — Local Loopback Telemetry Dashboard\n\n\
Usage:\n  \
  agypl dashboard [action] [options]      Start or manage local 127.0.0.1 dashboard (alias: dash, ui, --dashboard)\n\n\
Actions:\n  \
  open                                    Start/reuse background daemon and open standalone app window\n  \
  status                                  Show running dashboard daemon & desktop shortcut status\n  \
  stop                                    Gracefully stop the background dashboard daemon\n  \
  install-shortcut                        Install app-menu / desktop shortcut (alias: install-app)\n  \
  uninstall-shortcut                      Safely remove fingerprinted shortcuts (alias: uninstall-app)\n\n\
Options:\n  \
  --port, -p <port>                       Listen port (default 3789, auto-fallback on collision)\n  \
  --host <addr>                           Bind address (must be 127.0.0.1)\n  \
  --app / --browser                       Prefer frameless app window / default browser tab\n  \
  --desktop                               Also install/remove Desktop shortcut copy\n  \
  --no-open                               Do not open browser automatically\n  \
  --no-sync                               Skip automatic ledger sync on startup/queries\n  \
  --zh / --en / --lang <auto|zh|en>       Set initial UI language\n  \
  --idle-timeout <sec>                    Auto-exit seconds when no pages are active (bg default 600, fg 0)\n  \
  --background                            Run as background daemon worker\n  \
  --help, -h                              Show this help message\n"
        )
    }
}

fn should_auto_open(args: &DashboardCliArgs) -> bool {
    args.open && env::var("AGY_PULSE_NO_OPEN").ok().as_deref() != Some("1")
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

    match args.action {
        DashboardAction::Open => {
            if args.host != LOOPBACK_HOST {
                eprintln!(
                    "agy-pulse dashboard error: {}",
                    DashboardError::NonLoopbackHost(args.host)
                );
                return 1;
            }
            match launcher::ensure_background_dashboard(
                &args.host,
                args.port,
                args.idle_timeout_sec,
                resolved_lang,
                args.no_sync,
            ) {
                Ok(res) => {
                    if should_auto_open(&args) {
                        launcher::open_dashboard_window(&res.info.url, args.app_mode);
                    }
                    if is_zh {
                        let verb = if res.reused {
                            "已在运行"
                        } else {
                            "已在后台启动"
                        };
                        println!(
                            "agy-pulse 本地看板{verb}: {} (PID {})",
                            res.info.url, res.info.pid
                        );
                    } else {
                        let verb = if res.reused {
                            "already running"
                        } else {
                            "started in background"
                        };
                        println!(
                            "agy-pulse dashboard {verb}: {} (PID {})",
                            res.info.url, res.info.pid
                        );
                    }
                    0
                }
                Err(err) => {
                    eprintln!("agy-pulse dashboard error: {err}");
                    1
                }
            }
        }
        DashboardAction::Stop => match launcher::stop_background_dashboard(&args.host, args.port) {
            Ok(res) => {
                if res.stopped {
                    let pid = res.pid.unwrap_or(0);
                    let url = res
                        .url
                        .unwrap_or_else(|| format!("http://{}:{}", args.host, args.port));
                    if is_zh {
                        println!("已停止 agy-pulse 后台看板 (PID {pid}, {url})");
                    } else {
                        println!("Stopped background agy-pulse dashboard (PID {pid}, {url})");
                    }
                } else if is_zh {
                    println!("未检测到正在运行的 agy-pulse 后台看板。");
                } else {
                    println!("No running background agy-pulse dashboard found.");
                }
                0
            }
            Err(err) => {
                eprintln!("agy-pulse dashboard error: {err}");
                1
            }
        },
        DashboardAction::Status => {
            let running = launcher::find_running_dashboard(&args.host, args.port);
            let shortcut = launcher::get_desktop_shortcut_status();

            if let Some(info) = running {
                if is_zh {
                    println!("看板状态: 运行中 | URL: {} | PID: {}", info.url, info.pid);
                } else {
                    println!(
                        "Dashboard: running | URL: {} | PID: {}",
                        info.url, info.pid
                    );
                }
            } else if is_zh {
                println!("看板状态: 未运行");
            } else {
                println!("Dashboard: not running");
            }

            if shortcut.installed {
                let path_str = shortcut
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                if is_zh {
                    println!("桌面快捷方式: 已安装 ({path_str})");
                } else {
                    println!("Desktop shortcut: installed ({path_str})");
                }
            } else if is_zh {
                println!("桌面快捷方式: 未安装");
            } else {
                println!("Desktop shortcut: not installed");
            }
            0
        }
        DashboardAction::InstallShortcut => {
            let explicit_port = (args.port != DEFAULT_DASHBOARD_PORT).then_some(args.port);
            match launcher::install_desktop_shortcuts(args.include_desktop, explicit_port) {
                Ok(report) => {
                    if is_zh {
                        println!(
                            "已安装 agy-pulse Dashboard 桌面快捷方式: {}",
                            report.primary_path.display()
                        );
                        println!("现在可直接在系统应用菜单/桌面点击图标打开看板。");
                    } else {
                        println!(
                            "Installed agy-pulse Dashboard desktop shortcut: {}",
                            report.primary_path.display()
                        );
                        println!(
                            "You can now launch the dashboard directly from your app menu or desktop icon."
                        );
                    }
                    0
                }
                Err(err) => {
                    eprintln!("agy-pulse dashboard error: {err}");
                    1
                }
            }
        }
        DashboardAction::UninstallShortcut => {
            let report = launcher::uninstall_desktop_shortcuts();
            if !report.removed.is_empty() {
                let joined: Vec<String> = report
                    .removed
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                if is_zh {
                    println!(
                        "已移除 agy-pulse Dashboard 快捷方式 ({} 项): {}",
                        report.removed.len(),
                        joined.join(", ")
                    );
                } else {
                    println!(
                        "Removed agy-pulse Dashboard shortcut ({}): {}",
                        report.removed.len(),
                        joined.join(", ")
                    );
                }
            } else if is_zh {
                println!("未发现由 agy-pulse 创建的桌面快捷方式。");
            } else {
                println!("No agy-pulse Dashboard desktop shortcuts found to remove.");
            }
            0
        }
        DashboardAction::Serve => {
            platform::install_shutdown_signal_handler();
            let auto_open = should_auto_open(&args);
            let app_mode = args.app_mode;
            let idle_timeout_ms = args.idle_timeout_sec.saturating_mul(1000);

            if args.background_serve
                && args.port > 0
                && launcher::probe_dashboard_health(LOOPBACK_HOST, args.port).is_some()
            {
                launcher::release_dashboard_spawn_lock();
                return 0;
            }

            let cfg = DashboardServerConfig {
                host: args.host,
                port: args.port,
                lang: resolved_lang,
                no_sync: args.no_sync,
                defer_initial_sync: args.background_serve,
                idle_timeout_ms,
                ..Default::default()
            };

            let mut server = match DashboardServer::start(cfg) {
                Ok(s) => s,
                Err(err) => {
                    launcher::release_dashboard_spawn_lock();
                    eprintln!("agy-pulse dashboard error: {err}");
                    return 1;
                }
            };

            let info = server.info().clone();
            let owner_pid = std::process::id();
            let state = DashboardStateFile {
                app: DASHBOARD_APP_ID.to_string(),
                pid: owner_pid,
                port: info.port,
                host: info.host.clone(),
                url: info.url.clone(),
                idle_timeout_ms,
                started_at: info.started_at.clone(),
            };
            if let Err(err) = launcher::write_dashboard_state(&state)
                && args.background_serve
            {
                eprintln!("agy-pulse dashboard warning: failed to write state file: {err}");
            }
            launcher::release_dashboard_spawn_lock();

            if args.background_serve {
                server.wait_until_stopped();
                launcher::clear_dashboard_state_if_owner(owner_pid);
                return 0;
            }

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

            if auto_open {
                launcher::open_dashboard_window(&info.url, app_mode);
            }

            server.wait_until_stopped();
            launcher::clear_dashboard_state_if_owner(owner_pid);
            0
        }
    }
}
