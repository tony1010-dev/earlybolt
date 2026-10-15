//! OS-specific bits: finding PATH, running commands, keeping awake, wake schedules, sign-in terminals

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

pub fn home() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    PathBuf::from(std::env::var(var).unwrap_or_default())
}

/// A Command that never flashes a console window on Windows
pub fn command(exe: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(exe);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

// MARK: PATH

/// The app runs outside a terminal, so rebuild the PATH the user has in their shell.
/// Recomputed every time so a CLI installed a moment ago is found.
pub fn user_path() -> String {
    let mut parts: Vec<String> = Vec::new();
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut c = command(&shell);
        c.args(["-lic", r#"printf "__PATH__%s\n" "$PATH""#]);
        if let Some((_, out)) = run_with_timeout(c, Duration::from_secs(15)) {
            if let Some(p) = out.lines().filter_map(|l| l.split_once("__PATH__").map(|x| x.1)).last() {
                parts.push(p.to_string());
            }
        }
        let h = home().display().to_string();
        for d in ["/opt/homebrew/bin", "/usr/local/bin", &format!("{h}/.local/bin"), &format!("{h}/.grok/bin"),
                  &format!("{h}/.npm-global/bin"), &format!("{h}/.bun/bin"), "/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
            parts.push(d.to_string());
        }
        parts.join(":")
    }
    #[cfg(windows)]
    {
        // Installers update the user PATH in the registry; our own process PATH is stale, so read the registry
        for key in [r"HKCU\Environment", r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment"] {
            let mut c = command("reg");
            c.args(["query", key, "/v", "Path"]);
            if let Some((_, out)) = run_with_timeout(c, Duration::from_secs(10)) {
                for line in out.lines() {
                    if let Some(i) = line.find("REG_") {
                        let rest = &line[i..];
                        if let Some(v) = rest.splitn(2, char::is_whitespace).nth(1) {
                            parts.push(expand_env(v.trim()));
                        }
                    }
                }
            }
        }
        parts.push(std::env::var("PATH").unwrap_or_default());
        let h = home().display().to_string();
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let roaming = std::env::var("APPDATA").unwrap_or_default();
        for d in [format!(r"{h}\.local\bin"), format!(r"{local}\Programs\OpenAI\Codex\bin"),
                  format!(r"{h}\.grok\bin"), format!(r"{local}\agy\bin"), format!(r"{roaming}\npm")] {
            parts.push(d);
        }
        parts.join(";")
    }
}

#[cfg(windows)]
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(a) = rest.find('%') {
        out.push_str(&rest[..a]);
        let after = &rest[a + 1..];
        match after.find('%') {
            Some(b) => {
                let name = &after[..b];
                out.push_str(&std::env::var(name).unwrap_or_else(|_| format!("%{name}%")));
                rest = &after[b + 1..];
            }
            None => {
                out.push_str(&rest[a..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Look up an executable on PATH (with .exe/.cmd/.bat on Windows)
pub fn find_exe(name: &str, path: &str) -> Option<PathBuf> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
    for dir in path.split(sep).filter(|d| !d.is_empty()) {
        for ext in exts {
            let p = Path::new(dir).join(format!("{name}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

// MARK: Running commands

fn kill_tree(child: &mut Child) {
    let pid = child.id().to_string();
    #[cfg(not(windows))]
    let _ = command("/usr/bin/pkill").args(["-TERM", "-P", &pid]).status();
    #[cfg(windows)]
    let _ = command("taskkill").args(["/T", "/F", "/PID", &pid]).status();
    let _ = child.kill();
}

/// Run while collecting stdout+stderr; on timeout kill the child and its children
pub fn run_with_timeout(mut c: Command, timeout: Duration) -> Option<(bool, String)> {
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().ok()?;
    let mut so = child.stdout.take()?;
    let mut se = child.stderr.take()?;
    let t1 = thread::spawn(move || { let mut s = String::new(); let _ = so.read_to_string(&mut s); s });
    let t2 = thread::spawn(move || { let mut s = String::new(); let _ = se.read_to_string(&mut s); s });
    let start = Instant::now();
    let ok = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) if start.elapsed() > timeout => {
                kill_tree(&mut child);
                let _ = child.wait();
                break false;
            }
            Ok(None) => thread::sleep(Duration::from_millis(200)),
            Err(_) => break false,
        }
    };
    let out = format!("{}{}", t1.join().unwrap_or_default(), t2.join().unwrap_or_default());
    Some((ok, out))
}

/// Resolve argv[0] on PATH and run it
pub fn run_tool(argv: &[String], path: &str, cwd: &Path, timeout: Duration) -> (bool, String) {
    let Some(exe) = find_exe(&argv[0], path) else {
        return (false, format!("{} not found", argv[0]));
    };
    // Spawning fails if the working directory does not exist yet (fresh install)
    let _ = std::fs::create_dir_all(cwd);
    let mut c = command(exe);
    c.args(&argv[1..]).env("PATH", path).current_dir(cwd);
    run_with_timeout(c, timeout).unwrap_or((false, "failed to start".into()))
}

/// Run an official install script (macOS: bash, Windows: PowerShell)
pub fn run_installer(sh: &str, ps1: &str, path: &str) -> (bool, String) {
    #[cfg(not(windows))]
    let c = {
        let _ = ps1;
        let mut c = command("/bin/bash");
        c.args(["-c", sh]);
        c
    };
    #[cfg(windows)]
    let c = {
        let _ = sh;
        let mut c = command("powershell");
        c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", ps1]);
        c
    };
    let mut c = c;
    // Keep the Codex installer from asking questions
    c.env("PATH", path).env("CODEX_NON_INTERACTIVE", "1");
    run_with_timeout(c, Duration::from_secs(600)).unwrap_or((false, "could not run the installer".into()))
}

/// Run a sign-in command in a visible terminal (the CLI prints its browser sign-in prompts there)
pub fn open_login_terminal(login: &str, path: &str, ko: bool) -> Result<(), String> {
    let msg = if ko {
        "Earlybolt: 브라우저에서 로그인을 마치면 이 창은 닫아도 됩니다."
    } else {
        "Earlybolt: finish signing in in your browser, then you can close this window."
    };
    #[cfg(not(windows))]
    {
        let script = format!("export PATH='{}'; clear; echo '{msg}'; echo; {login}", path.replace('\'', ""));
        let r = command("/usr/bin/osascript")
            .args(["-e", &format!("tell application \"Terminal\" to do script \"{}\"", script.replace('"', "\\\"")),
                   "-e", "tell application \"Terminal\" to activate"])
            .status();
        r.map(|_| ()).map_err(|e| e.to_string())
    }
    #[cfg(windows)]
    {
        // Antigravity prints a link and waits for a code instead of opening the browser
        let extra = if !login.starts_with("agy") { "" } else if ko {
            "Write-Host '아래 링크를 Ctrl+클릭해 로그인하고, 브라우저에 표시된 코드를 60초 안에 여기 붙여넣으세요.' -ForegroundColor Yellow; "
        } else {
            "Write-Host 'Ctrl+click the link below, sign in, then paste the code shown in the browser here within 60 seconds.' -ForegroundColor Yellow; "
        };
        let ps = format!("$env:Path = '{}'; Write-Host '{msg}'; {extra}Write-Host ''; {login}", path.replace('\'', "''"));
        Command::new("powershell")
            .args(["-NoExit", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps])
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

// MARK: Keeping awake

/// Prevents system sleep while alive (the display may still turn off)
pub struct Awake {
    #[cfg(not(windows))]
    child: Option<Child>,
}

impl Awake {
    pub fn hold() -> Awake {
        #[cfg(not(windows))]
        {
            let pid = std::process::id().to_string();
            Awake { child: command("/usr/bin/caffeinate").args(["-i", "-w", &pid]).spawn().ok() }
        }
        #[cfg(windows)]
        {
            set_thread_awake(true);
            Awake {}
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        #[cfg(not(windows))]
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        #[cfg(windows)]
        set_thread_awake(false);
    }
}

/// On Windows this is per-thread, so call it from the long-lived scheduler thread
#[cfg(windows)]
pub fn set_thread_awake(on: bool) {
    use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED};
    unsafe {
        SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
    }
}

// MARK: Laptop wake schedule

/// `times` are minutes after midnight. macOS supports one repeating wake per day, so only the first is used.
pub fn set_wake(times: &[u32], on: bool, ko: bool) -> Result<String, String> {
    let t = |en: &str, k: &str| if ko { k.to_string() } else { en.to_string() };
    #[cfg(not(windows))]
    {
        let first = times.first().copied().unwrap_or(0);
        let cmd = if on {
            format!("pmset repeat wakeorpoweron MTWRFSU {:02}:{:02}:00", first / 60, first % 60)
        } else {
            "pmset repeat cancel".into()
        };
        let st = command("/usr/bin/osascript")
            .args(["-e", &format!("do shell script \"{cmd}\" with administrator privileges")])
            .status()
            .map_err(|e| e.to_string())?;
        if !st.success() {
            return Err(t("Not set (cancelled)", "설정하지 못했어요 (취소됨)"));
        }
        let hm = format!("{:02}:{:02}", first / 60, first % 60);
        Ok(if on { t(&format!("Wakes daily at {hm}"), &format!("매일 {hm}에 자동으로 깨어납니다")) }
           else { t("Wake schedule removed", "자동 깨우기를 해제했어요") })
    }
    #[cfg(windows)]
    {
        // Task Scheduler: a no-op task that wakes the PC before each ping (no admin rights needed)
        let name = r"Earlybolt\Wake";
        let _ = command("schtasks").args(["/Delete", "/TN", name, "/F"]).status();
        if !on {
            return Ok(t("Wake schedule removed", "자동 깨우기를 해제했어요"));
        }
        let triggers: String = times.iter().map(|m| format!(
            "<CalendarTrigger><StartBoundary>2026-01-01T{:02}:{:02}:00</StartBoundary><Enabled>true</Enabled>\
             <ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay></CalendarTrigger>", m / 60, m % 60)).collect();
        let xml = format!(r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Earlybolt: wakes the PC 1 minute before each scheduled ping</Description></RegistrationInfo>
  <Triggers>{triggers}</Triggers>
  <Principals><Principal id="Author"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <WakeToRun>true</WakeToRun>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>false</StartWhenAvailable>
    <ExecutionTimeLimit>PT5M</ExecutionTimeLimit>
    <Hidden>true</Hidden>
  </Settings>
  <Actions Context="Author"><Exec><Command>cmd.exe</Command><Arguments>/c exit</Arguments></Exec></Actions>
</Task>"#);
        // schtasks requires UTF-16 XML
        let file = std::env::temp_dir().join("earlybolt-wake.xml");
        let mut bytes = vec![0xFF, 0xFE];
        for u in xml.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
        let (ok, out) = run_with_timeout(
            { let mut c = command("schtasks"); c.args(["/Create", "/TN", name, "/XML", &file.display().to_string(), "/F"]); c },
            Duration::from_secs(20),
        ).unwrap_or((false, String::new()));
        let _ = std::fs::remove_file(&file);
        if ok {
            Ok(t(&format!("Wakes 1 min before each ping ({}×/day)", times.len()),
                 &format!("호출 1분 전마다 PC를 깨웁니다 (하루 {}회)", times.len())))
        } else {
            Err(t(&format!("Task Scheduler failed: {}", out.trim()), &format!("작업 스케줄러 등록 실패: {}", out.trim())))
        }
    }
}

/// Stop pre-release builds (WoongToken / AIWindowPing scripts) so the same slot is not pinged twice
pub fn cleanup_legacy() {
    #[cfg(target_os = "macos")]
    {
        let uid = command("/usr/bin/id").arg("-u").output().ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        let la = home().join("Library/LaunchAgents");
        for label in ["com.woongtoken.app", "com.aiwindowping.agent"] {
            let _ = command("/bin/launchctl").args(["bootout", &format!("gui/{uid}/{label}")]).status();
            let _ = std::fs::remove_file(la.join(format!("{label}.plist")));
        }
        let _ = command("/usr/bin/pkill").args(["-x", "WoongToken"]).status();
    }
}
