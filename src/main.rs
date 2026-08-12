use serde::{Deserialize, Serialize};
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
const SYSTEMD_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const LAUNCHD_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

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
    target: String,
    command: Vec<OsString>,
    contract: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct ExecutionContract {
    version: u8,
    target: String,
    command: Vec<String>,
    working_directory: PathBuf,
    #[serde(default)]
    shell: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    required_env: Vec<String>,
    #[serde(default)]
    log: Option<PathBuf>,
}

fn usage() {
    println!(
        "apux — preflight unattended commands\n\nUSAGE:\n  apux <check|diff|run|init> --target <cron|systemd|launchd> -- <command> [args...]\n  apux verify --target <cron|systemd|launchd> [--contract apux.yaml]\n\nEXAMPLES:\n  apux check --target cron -- ./scripts/nightly-sync.sh\n  apux check --target systemd -- /usr/local/bin/nightly-sync\n  apux check --target launchd -- /usr/local/bin/nightly-sync\n  apux verify --target cron"
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
    if !matches!(
        action.as_str(),
        "check" | "diff" | "run" | "init" | "verify"
    ) {
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
    if !matches!(target.as_str(), "cron" | "systemd" | "launchd") {
        return Err(format!(
            "unsupported target: {target}. Supported targets: cron, systemd, launchd."
        ));
    }
    if action == "verify" {
        let next = args.next();
        let contract = match next.as_deref() {
            None => PathBuf::from("apux.yaml"),
            Some(flag) if flag == std::ffi::OsStr::new("--contract") => {
                PathBuf::from(args.next().ok_or("missing path after --contract")?)
            }
            Some(_) => return Err("verify accepts only optional --contract <path>".into()),
        };
        return Ok(Invocation {
            action,
            target,
            command: vec![],
            contract,
        });
    }
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        return Err("place the command after --".into());
    }
    let command: Vec<_> = args.collect();
    if command.is_empty() {
        return Err("missing command after --".into());
    }
    Ok(Invocation {
        action,
        target,
        command,
        contract: PathBuf::from("apux.yaml"),
    })
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

fn systemd_findings(command: &[OsString]) -> Vec<Finding> {
    let mut out = Vec::new();
    let program = command_path(command);
    let program_s = program.to_string_lossy();
    if !program.is_absolute() {
        out.push(Finding::error(
            "APX030",
            format!(
                "systemd ExecStart requires an absolute executable path; `{program_s}` is relative"
            ),
            "use an absolute executable path, e.g. /usr/local/bin/job",
        ));
    } else if !program.is_file() {
        out.push(Finding::error(
            "APX001",
            format!("command path `{program_s}` does not exist"),
            "check the path on the service host",
        ));
    }
    #[cfg(unix)]
    if program.is_file() {
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
    if command.iter().skip(1).any(|arg| {
        matches!(
            arg.to_string_lossy().as_ref(),
            "|" | "&&" | "||" | ";" | ">" | ">>"
        )
    }) {
        out.push(Finding::warning(
            "APX031",
            "systemd does not run ExecStart through a shell",
            "use a wrapper script for pipes, redirects, or shell control operators",
        ));
    }
    if let Some(content) = file_contents(command) {
        let refs: Vec<_> = referenced_env(&content)
            .into_iter()
            .filter(|n| !["PATH", "HOME", "PWD", "SHELL", "USER"].contains(&n.as_str()))
            .collect();
        if !refs.is_empty() {
            out.push(Finding::warning("APX005", format!("script references environment variables a systemd service will not inherit: {}", refs.join(", ")), "set Environment= or EnvironmentFile= explicitly in the unit"));
        }
        if content.contains("./") || content.contains("../") {
            out.push(Finding::warning(
                "APX009",
                "script uses relative paths",
                "set WorkingDirectory= in the unit",
            ));
        }
    }
    out.push(Finding::info("APX032", "systemd sends standard output and error to the journal by default; inspect with journalctl -u <unit>"));
    out
}

fn print_systemd_findings(command: &[OsString]) -> bool {
    println!(
        "Apux preflight: systemd\n  command: {}\n  shell:   none (ExecStart runs directly)\n  PATH:    {SYSTEMD_PATH}\n",
        command_display(command)
    );
    let results = systemd_findings(command);
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

fn launchd_findings(command: &[OsString]) -> Vec<Finding> {
    let mut out = Vec::new();
    let program = command_path(command);
    let program_s = program.to_string_lossy();
    if !program.is_absolute() {
        out.push(Finding::error("APX040", format!("launchd ProgramArguments[0] should be an absolute executable path; `{program_s}` is relative"), "use an absolute executable path in the launchd plist"));
    } else if !program.is_file() {
        out.push(Finding::error(
            "APX001",
            format!("command path `{program_s}` does not exist"),
            "check the path on the Mac that runs launchd",
        ));
    }
    if command.iter().skip(1).any(|arg| {
        matches!(
            arg.to_string_lossy().as_ref(),
            "|" | "&&" | "||" | ";" | ">" | ">>"
        )
    }) {
        out.push(Finding::warning(
            "APX041",
            "launchd does not run ProgramArguments through a shell",
            "put shell control operators in a wrapper script and invoke that script",
        ));
    }
    if let Some(content) = file_contents(command) {
        let refs: Vec<_> = referenced_env(&content)
            .into_iter()
            .filter(|n| !["PATH", "HOME", "PWD", "SHELL", "USER"].contains(&n.as_str()))
            .collect();
        if !refs.is_empty() {
            out.push(Finding::warning(
                "APX005",
                format!(
                    "script references environment variables launchd will not inherit: {}",
                    refs.join(", ")
                ),
                "declare EnvironmentVariables in the launchd plist or load values in a wrapper",
            ));
        }
        if content.contains("./") || content.contains("../") {
            out.push(Finding::warning(
                "APX009",
                "script uses relative paths",
                "set WorkingDirectory in the plist or cd in a wrapper",
            ));
        }
    }
    out.push(Finding::info(
        "APX042",
        "declare StandardOutPath and StandardErrorPath in the plist for durable logs",
    ));
    out
}

fn print_launchd_findings(command: &[OsString]) -> bool {
    println!(
        "Apux preflight: launchd\n  command: {}\n  shell:   none (ProgramArguments runs directly)\n  PATH:    {LAUNCHD_PATH}\n",
        command_display(command)
    );
    let results = launchd_findings(command);
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

fn print_target_findings(target: &str, command: &[OsString]) -> bool {
    match target {
        "cron" => print_findings(command),
        "systemd" => print_systemd_findings(command),
        "launchd" => print_launchd_findings(command),
        _ => unreachable!(),
    }
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

fn read_contract(path: &Path) -> Result<ExecutionContract, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    serde_yaml::from_str(&contents).map_err(|error| format!("invalid {}: {error}", path.display()))
}

fn verify_contract(path: &Path, target: &str) -> bool {
    let contract = match read_contract(path) {
        Ok(contract) => contract,
        Err(error) => {
            println!("ERROR APX020: {error}");
            return true;
        }
    };
    let mut has_errors = false;
    println!(
        "Apux contract verification: {}\n  contract: {}\n",
        target,
        path.display()
    );
    if contract.version != 1 {
        println!(
            "ERROR APX021: unsupported contract version {}",
            contract.version
        );
        has_errors = true;
    }
    if contract.target != target {
        println!(
            "ERROR APX022: contract target `{}` does not match `--target {target}`",
            contract.target
        );
        has_errors = true;
    }
    if contract.command.is_empty() {
        println!("ERROR APX023: contract command is empty");
        return true;
    }
    if !contract.working_directory.is_dir() {
        println!(
            "ERROR APX024: working directory `{}` does not exist",
            contract.working_directory.display()
        );
        has_errors = true;
    }
    if let Some(shell) = &contract.shell
        && !Path::new(shell).exists()
        && shell.starts_with('/')
    {
        println!("ERROR APX025: declared shell `{shell}` does not exist");
        has_errors = true;
    }
    let command: Vec<OsString> = contract.command.iter().map(OsString::from).collect();
    has_errors |= print_target_findings(target, &command);
    if !contract.required_env.is_empty() {
        println!(
            "INFO APX026: required environment names are declared: {}",
            contract.required_env.join(", ")
        );
        println!(
            "  ensure your scheduler injects these values; Apux never reads or stores their values."
        );
    }
    if contract.log.is_none() {
        println!("WARN APX027: contract has no declared log destination");
    }
    has_errors
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
    let contract = ExecutionContract {
        version: 1,
        target: "cron".into(),
        command: command
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect(),
        working_directory: root.clone(),
        shell: Some("/usr/bin/env bash".into()),
        path: Some(CRON_PATH.into()),
        required_env: vec![],
        log: Some(log_path),
    };
    fs::write(
        root.join("apux.yaml"),
        serde_yaml::to_string(&contract).map_err(io::Error::other)?,
    )?;
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
            if print_target_findings(&invocation.target, &invocation.command) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        "diff" => {
            if invocation.target == "cron" {
                diff(&invocation.command);
            } else if invocation.target == "systemd" {
                println!(
                    "Apux context diff: local → systemd\n  PATH: local value → {SYSTEMD_PATH}\n  SHELL: local value → <absent>\n  TERM: local value → <absent>\n  working directory: local value → / unless WorkingDirectory= is declared"
                );
            } else {
                println!(
                    "Apux context diff: local → launchd\n  PATH: local value → {LAUNCHD_PATH}\n  SHELL: local value → <absent>\n  TERM: local value → <absent>\n  working directory: local value → / unless WorkingDirectory is declared\n  environment: terminal exports → only EnvironmentVariables declared in the plist"
                );
            }
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
        "init" if invocation.target == "cron" => match init(&invocation.command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: could not create Apux files: {error}");
                ExitCode::from(1)
            }
        },
        "init" => {
            eprintln!(
                "error: `init` currently generates cron wrappers only; use check and verify for {} today",
                invocation.target
            );
            ExitCode::from(2)
        }
        "verify" => {
            if verify_contract(&invocation.contract, &invocation.target) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
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

    #[test]
    fn parses_execution_contract() {
        let value = "version: 1\ntarget: cron\ncommand:\n  - /bin/echo\n  - hello\nworking_directory: /tmp\nrequired_env:\n  - API_TOKEN\n";
        let contract: ExecutionContract = serde_yaml::from_str(value).unwrap();
        assert_eq!(contract.command, ["/bin/echo", "hello"]);
        assert_eq!(contract.required_env, ["API_TOKEN"]);
    }
}
