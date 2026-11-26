use crate::ledger::{LedgerError, LedgerStore, validate_and_normalize_account_email};

fn print_account_help() {
    println!(
        "Usage: agypl account <subcommand> [options]\n\n\
         Subcommands:\n  \
           assign <email> [--all-unassigned | --workspace <substr>] [--dry-run]\n      \
               Assign local 'unassigned' conversations and steps to <email>\n\n\
         Options:\n  \
           -h, --help    Show help information"
    );
}

fn print_assign_help() {
    println!(
        "Usage: agypl account assign <email> [--all-unassigned] [--workspace <substr>] [--dry-run]\n\n\
         Reassign local 'unassigned' conversations and steps on this device to <email>.\n\
         Already-assigned conversations and remote device records are never modified.\n\n\
         Options:\n  \
           --all-unassigned        Assign all local unassigned conversations (default)\n  \
           --workspace <substr>    Only assign unassigned conversations whose workspace contains <substr>\n  \
           --dry-run               Preview matching counts without modifying the ledger database\n  \
           -h, --help              Show this help message"
    );
}

struct AssignArgs {
    email: String,
    workspace_substr: Option<String>,
    dry_run: bool,
}

fn parse_assign_args(args: &[String]) -> Result<AssignArgs, String> {
    let mut email: Option<String> = None;
    let mut all_unassigned = false;
    let mut workspace_substr: Option<String> = None;
    let mut dry_run = false;

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--all-unassigned" {
            all_unassigned = true;
        } else if arg == "--dry-run" {
            dry_run = true;
        } else if let Some(val) = arg.strip_prefix("--workspace=") {
            if val.trim().is_empty() {
                return Err("--workspace requires a non-empty substring".to_string());
            }
            workspace_substr = Some(val.to_string());
        } else if arg == "--workspace" {
            let Some(next) = args.get(i + 1) else {
                return Err("--workspace requires a value".to_string());
            };
            if next.trim().is_empty() {
                return Err("--workspace requires a non-empty substring".to_string());
            }
            workspace_substr = Some(next.clone());
            i += 1;
        } else if arg.starts_with('-') {
            return Err(format!("unknown option '{arg}'"));
        } else if email.is_none() {
            email = Some(arg.to_string());
        } else {
            return Err(format!("unexpected extra argument '{arg}'"));
        }
        i += 1;
    }

    if all_unassigned && workspace_substr.is_some() {
        return Err("--all-unassigned cannot be combined with --workspace".to_string());
    }

    let raw_email = email.ok_or_else(|| "missing required <email> argument".to_string())?;
    let normalized = validate_and_normalize_account_email(&raw_email)
        .ok_or_else(|| format!("invalid email '{raw_email}'; expected <name>@<domain>"))?;

    Ok(AssignArgs {
        email: normalized,
        workspace_substr,
        dry_run,
    })
}

fn run_account_assign(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_assign_help();
        return 0;
    }

    let parsed = match parse_assign_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("agy-pulse account assign error: {msg}");
            eprintln!("Run 'agy-pulse account assign --help' for usage.");
            return 2;
        }
    };

    let store = match LedgerStore::open_default() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("agy-pulse account assign error: {err}");
            return 1;
        }
    };

    match store.assign_unassigned_account(
        &parsed.email,
        parsed.workspace_substr.as_deref(),
        parsed.dry_run,
    ) {
        Ok(report) => {
            if report.dry_run {
                println!(
                    "Dry run: would assign {} conversation(s) and {} step(s) to {} (no database changes made)",
                    report.conversations_updated, report.steps_updated, report.account_id
                );
            } else {
                println!(
                    "Assigned {} conversation(s) and {} step(s) to {}",
                    report.conversations_updated, report.steps_updated, report.account_id
                );
            }
            0
        }
        Err(LedgerError::InvalidAccountEmail(msg)) => {
            eprintln!("agy-pulse account assign error: invalid email '{msg}'");
            2
        }
        Err(err) => {
            eprintln!("agy-pulse account assign error: {err}");
            1
        }
    }
}

/// CLI entrypoint for `agypl account ...` / `agy-pulse account ...`.
pub fn run_account_cli(args: &[String]) -> i32 {
    let sub = args.get(1).map(String::as_str);
    match sub {
        Some("assign") => run_account_assign(&args[2..]),
        Some("help" | "--help" | "-h") => {
            print_account_help();
            0
        }
        Some(other) => {
            eprintln!("agy-pulse account error: unknown subcommand '{other}'");
            eprintln!("Run 'agy-pulse account --help' for usage.");
            2
        }
        None => {
            eprintln!("agy-pulse account error: missing subcommand");
            eprintln!("Run 'agy-pulse account --help' for usage.");
            2
        }
    }
}
