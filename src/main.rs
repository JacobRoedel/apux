use std::{
    collections::BTreeSet,
    env,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

const CRON_PATH: &str = "/usr/bin:/bin";
const CRON_SHELL: &str = "/bin/sh";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Error,
    Warning,
    Info,
}

#[derive(Debug)]
struct Finding {
    level: Level,
    code: &'static str,
    message: String,
    remedy: Option<String>,
}

impl Finding {
    fn error(code: &'static str, message: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            level: Level::Error,
            code,
            message: message.into(),
            remedy: Some(remedy.into()),
        }
    }
    fn warning(code: &'static str, message: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            code,
            message: message.into(),
            remedy: Some(remedy.into()),
        }
    }
    fn info(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            level: Level::Info,
            code,
            message: message.into(),
            remedy: None,
        }
    }
}

#[derive(Debug)]
struct Invocation {
    action: String,
    command: Vec<OsString>,
}

fn usage() {
    println!(
        "apux — preflight unattended commands\n\nUSAGE:\n  apux <check|diff|run|init> --target cron -- <command> [args...]\n\nEXAMPLES:\n  apux check --target cron -- ./scripts/nightly-sync.sh\n  apux run --target cron -- node scripts/sync.js"
    );
}

fn parse_args() -> Result<Invocation, String> {
    let mut args = env::args_os().skip(1);
    let Some(action) = args.next() else {
        return Err("missing command".into());
    };
    if action == "--help" || action == "-h" {
        usage();
        std::process::exit(0);
    }
    let action = action.to_string_lossy().to_string();
    if !matches!(action.as_str(), "check" | "diff" | "run" | "init") {
        return Err(format!("unknown command: {action}"));
    }
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--target")) {
        return Err("expected --target cron".into());
    }
    let target = args
        .next()
        .ok_or("missing target after --target")?
        .to_string_lossy()
        .to_string();
    if target != "cron" {
        return Err(format!(
            "unsupported target: {target}. The MVP supports cron."
        ));
    }
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        return Err("place the command after --".into());
    }
    let command: Vec<_> = args.collect();
    if command.is_empty() {
        return Err("missing command after --".into());
    }
    Ok(Invocation { action, command })
}

fn command_display(command: &[OsString]) -> String {
    command
        .iter()
        .map(|a| shell_quote(&a.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    if value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"_./:-=".contains(&b))
    {
        value.into()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn command_path(command: &[OsString]) -> PathBuf {
    PathBuf::from(&command[0])
}

fn file_contents(command: &[OsString]) -> Option<String> {
    let path = command_path(command);
    fs::read_to_string(path).ok()
}

fn referenced_env(content: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let bytes = content.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            index += 1;
            continue;
        }
        index += 1;
        if index < bytes.len() && bytes[index] == b'{' {
            index += 1;
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            if index < bytes.len() && bytes[index] == b'}' && index > start {
                names.insert(content[start..index].into());
            }
        } else {
            let start = index;
            if index >= bytes.len() || !(bytes[index].is_ascii_alphabetic() || bytes[index] == b'_')
            {
                continue;
            }
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            if index > start {
                names.insert(content[start..index].into());
            }
        }
    }
    names
}

fn is_cron_path_executable(name: &str) -> bool {
    if name.contains('/') {
        return Path::new(name).is_file();
    }
    CRON_PATH
        .split(':')
        .map(|dir| Path::new(dir).join(name))
        .any(|path| path.is_file())
}

fn findings(command: &[OsString]) -> Vec<Finding> {
    let mut out = Vec::new();
    let program = command_path(command);
    let program_s = program.to_string_lossy();
    if program_s.starts_with("./") || !program.is_absolute() && program_s.contains('/') {
        out.push(Finding::warning(
            "APX006",
            format!("relative command path `{program_s}` depends on cron's working directory"),
            "set an absolute command path or use Apux's generated wrapper",
        ));
    }
    if program_s.contains('~') {
        out.push(Finding::warning(
            "APX007",
            "`~` expansion depends on HOME",
            "use an absolute path",
        ));
    }
    if program.exists() && program.is_file() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(&program)
                .map(|m| m.permissions().mode() & 0o111 == 0)
                .unwrap_or(false)
            {
                out.push(Finding::warning(
                    "APX002",
                    format!("`{program_s}` is not executable"),
                    "run chmod +x, or invoke it through its interpreter",
                ));
            }
        }
    } else if !program_s.contains('/') && !is_cron_path_executable(&program_s) {
        if let Some(local) = find_on_path(&program_s) {
            out.push(Finding::warning("APX008", format!("`{program_s}` resolves locally to `{}` but is not in cron's PATH ({CRON_PATH})", local.display()), "set PATH explicitly in a wrapper"));
        } else {
            out.push(Finding::error(
                "APX001",
                format!("cannot find command `{program_s}`"),
                "use an absolute path or install the command",
            ));
        }
    } else if program_s.contains('/') {
        out.push(Finding::error(
            "APX001",
            format!("command path `{program_s}` does not exist"),
            "check the path from the scheduler's machine",
        ));
    }

    if let Some(content) = file_contents(command) {
        let first_line = content.lines().next().unwrap_or_default();
        if !first_line.starts_with("#!") {
            out.push(Finding::warning(
                "APX003",
                "script has no shebang",
                "add a shebang such as #!/usr/bin/env bash",
            ));
        }
        if (content.contains("[[") || content.contains("source ") || content.contains("function "))
            && !first_line.contains("bash")
        {
            out.push(Finding::warning(
                "APX004",
                "script appears to use Bash syntax but does not declare Bash",
                "use #!/usr/bin/env bash; cron otherwise defaults to /bin/sh",
            ));
        }
        let ignored = ["PATH", "HOME", "PWD", "SHELL", "USER", "?", "#", "$", "0"];
        let refs: Vec<_> = referenced_env(&content)
            .into_iter()
            .filter(|n| !ignored.contains(&n.as_str()))
            .collect();
        if !refs.is_empty() {
            out.push(Finding::warning(
                "APX005",
                format!(
                    "script references environment variables cron will not inherit: {}",
                    refs.join(", ")
                ),
                "declare required variables in a wrapper or scheduler configuration",
            ));
        }
        if content.contains("./") || content.contains("../") {
            out.push(Finding::warning(
                "APX009",
                "script uses relative paths",
                "set an explicit working directory before running commands",
            ));
        }
        if content.contains("~/") {
            out.push(Finding::warning(
                "APX010",
                "script expands `~`; cron's HOME may differ from your terminal",
                "use an absolute path or explicitly set HOME",
            ));
        }
        if !content.contains(">") && !content.contains("logger ") {
            out.push(Finding::info("APX011", "no log redirection detected; cron may email output or discard it depending on configuration"));
        }
    }
    out
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(|p| Path::new(p).join(name))
        .find(|p| p.is_file())
}

fn print_findings(command: &[OsString]) -> bool {
    println!(
        "Apux preflight: cron\n  command: {}\n  shell:   {CRON_SHELL}\n  PATH:    {CRON_PATH}\n",
        command_display(command)
    );
    let results = findings(command);
    if results.is_empty() {
        println!("✓ No obvious cron-context risks found.");
        return false;
    }
    for item in &results {
        let label = match item.level {
            Level::Error => "ERROR",
            Level::Warning => "WARN",
            Level::Info => "INFO",
        };
        println!("{label} {}: {}", item.code, item.message);
        if let Some(remedy) = &item.remedy {
            println!("  fix: {remedy}");
        }
    }
    results.iter().any(|f| f.level == Level::Error)
}

fn run_cron(command: &[OsString]) -> io::Result<i32> {
    let home = env::var_os("HOME").unwrap_or_else(|| OsString::from("/"));
    let status = Command::new(&command[0])
        .args(&command[1..])
        .env_clear()
        .env("PATH", CRON_PATH)
        .env("SHELL", CRON_SHELL)
        .env("HOME", home)
        .env(
            "LOGNAME",
            env::var("USER").unwrap_or_else(|_| "unknown".into()),
        )
        .current_dir(env::current_dir()?)
        .status()?;
    Ok(status.code().unwrap_or(1))
}

fn diff(command: &[OsString]) {
    println!(
        "Apux context diff: local → cron\n  command: {}\n",
        command_display(command)
    );
    for (name, cron) in [
        ("PATH", CRON_PATH),
        ("SHELL", CRON_SHELL),
        ("TERM", "<absent>"),
        ("PWD", "scheduler-defined"),
    ] {
        let local = env::var(name).unwrap_or_else(|_| "<absent>".into());
        if local != cron {
            println!("~ {name}\n  local: {local}\n  cron:  {cron}");
        }
    }
    if let Some(content) = file_contents(command) {
        let refs: Vec<_> = referenced_env(&content)
            .into_iter()
            .filter(|name| !["PATH", "HOME", "PWD", "SHELL", "USER"].contains(&name.as_str()))
            .collect();
        if !refs.is_empty() {
            println!(
                "\n! variables referenced by the script but absent from cron by default: {}",
                refs.join(", ")
            );
        }
    }
}

fn init(command: &[OsString]) -> io::Result<()> {
    let root = env::current_dir()?;
    let apux_dir = root.join(".apux");
    fs::create_dir_all(&apux_dir)?;
    let log_path = root.join(".apux/cron.log");
    let wrapper = format!(
        "#!/usr/bin/env bash\nset -euo pipefail\n\ncd {}\nexport PATH=\"{}\"\nexport HOME=\"${{HOME:-/}}\"\n\nexec {} >> {} 2>&1\n",
        shell_quote(&root.display().to_string()),
        CRON_PATH,
        command_display(command),
        shell_quote(&log_path.display().to_string())
    );
    let wrapper_path = apux_dir.join("run.sh");
    fs::write(&wrapper_path, wrapper)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&wrapper_path, fs::Permissions::from_mode(0o755))?;
    }
    let contract = format!(
        "version: 1\ntarget: cron\ncommand: {}\nworking_directory: {}\nshell: /usr/bin/env bash\npath: {}\nlog: {}\n",
        command_display(command),
        root.display(),
        CRON_PATH,
        log_path.display()
    );
    fs::write(root.join("apux.yaml"), contract)?;
    println!(
        "Created .apux/run.sh and apux.yaml.\n\nReview them, then add a schedule like:\n  0 2 * * * {}\n",
        wrapper_path.display()
    );
    Ok(())
}

fn main() -> ExitCode {
    let invocation = match parse_args() {
        Ok(i) => i,
        Err(error) => {
            eprintln!("error: {error}\n");
            usage();
            return ExitCode::from(2);
        }
    };
    match invocation.action.as_str() {
        "check" => {
            if print_findings(&invocation.command) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        "diff" => {
            diff(&invocation.command);
            ExitCode::SUCCESS
        }
        "run" => match run_cron(&invocation.command) {
            Ok(0) => ExitCode::SUCCESS,
            Ok(code) => ExitCode::from(code as u8),
            Err(error) => {
                eprintln!("error: failed to start command: {error}");
                ExitCode::from(1)
            }
        },
        "init" => match init(&invocation.command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: could not create Apux files: {error}");
                ExitCode::from(1)
            }
        },
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_braced_and_unbraced_environment_references() {
        assert_eq!(
            referenced_env("$TOKEN ${DATABASE_URL} $$ $1"),
            BTreeSet::from(["DATABASE_URL".into(), "TOKEN".into()])
        );
    }
    #[test]
    fn quotes_shell_arguments() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("bin/run"), "bin/run");
    }
}
