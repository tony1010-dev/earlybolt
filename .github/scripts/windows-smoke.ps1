# Windows smoke test for Earlybolt (see .github/workflows/windows-smoke.yml)
$ErrorActionPreference = 'Stop'
$exe = Resolve-Path 'src-tauri\target\release\earlybolt.exe'
$out = New-Item -ItemType Directory -Force 'smoke'
$failures = @()
function Fail($msg) { Write-Host "FAIL: $msg" -ForegroundColor Red; $script:failures += $msg }
function Pass($msg) { Write-Host "ok: $msg" -ForegroundColor Green }

Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type @'
using System; using System.Text; using System.Collections.Generic; using System.Runtime.InteropServices;
public static class Win {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  public static List<string> Visible() {
    var list = new List<string>();
    EnumWindows((h, l) => {
      if (!IsWindowVisible(h)) return true;
      var c = new StringBuilder(256); GetClassName(h, c, 256);
      var t = new StringBuilder(256); GetWindowText(h, t, 256);
      RECT r; GetWindowRect(h, out r);
      list.Add(h + "|" + c + "|" + t + "|" + r.Left + "," + r.Top + "," + r.Right + "," + r.Bottom);
      return true;
    }, IntPtr.Zero);
    return list;
  }
}
'@

function Shot($name) {
  $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
  $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
  [System.Drawing.Graphics]::FromImage($bmp).CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
  $bmp.Save("$out\$name.png")
}

# Console windows that appear while the app runs (checked every 50 ms by a separate process)
$watcher = Join-Path $out 'consoles.txt'
$before = ([Win]::Visible() | ForEach-Object { ($_ -split '\|')[0] }) -join ','
$watch = Start-Process pwsh -PassThru -WindowStyle Hidden -ArgumentList '-NoProfile', '-Command', @"
Add-Type @'
using System; using System.Text; using System.Runtime.InteropServices;
public static class W {
  public delegate bool P(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(P f, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
}
'@
`$known = @{}; foreach (`$h in '$before'.Split(',')) { `$known[`$h] = 1 }
while (`$true) {
  [W]::EnumWindows({ param(`$h, `$l)
    if ([W]::IsWindowVisible(`$h) -and -not `$known.ContainsKey([string]`$h)) {
      `$c = New-Object Text.StringBuilder 256; [void][W]::GetClassName(`$h, `$c, 256)
      if ('ConsoleWindowClass', 'CASCADIA_HOSTING_WINDOW_CLASS', 'PseudoConsoleWindow' -contains `$c.ToString()) {
        Add-Content '$watcher' "`$(Get-Date -Format HH:mm:ss.fff) `$h `$c"
      }
      `$known[[string]`$h] = 1
    }
    `$true }, [IntPtr]::Zero) | Out-Null
  Start-Sleep -Milliseconds 50
}
"@
Start-Sleep 3

# Fake CLIs in a folder with a Korean name, only on the user PATH in the registry (not in this process's PATH)
$bin = Join-Path $env:USERPROFILE '테스트 도구\bin'
New-Item -ItemType Directory -Force $bin | Out-Null
Set-Content "$bin\agy.cmd" -Encoding ascii -Value '@echo {"command":{"data":{"groups":[{"name":"Gemini 3","buckets":[{"remaining_fraction":0.7,"window":"5h","reset_time":"2030-01-01T00:00:00Z"},{"remaining_fraction":0.9,"window":"weekly","reset_time":"2030-01-05T00:00:00Z"}]}]}}}'
Set-Content "$bin\claude.cmd" -Encoding ascii -Value '@echo {"loggedIn": true}'
[Environment]::SetEnvironmentVariable('Path', "$bin;" + [Environment]::GetEnvironmentVariable('Path', 'User'), 'User')

# 1. --usage finds agy through the registry PATH and parses it
$usage = (& $exe --usage | Out-String)
Set-Content "$out\usage.json" $usage
if ($usage -match '"agy"' -and $usage -match '"used": 30') { Pass 'usage: agy found via registry PATH (Korean folder) and parsed' } else { Fail "usage output: $usage" }

# 2. --wake exits at once
$t = Measure-Command { Start-Process $exe -ArgumentList '--wake' -Wait }
if ($t.TotalSeconds -lt 5) { Pass "--wake exits in $([int]$t.TotalMilliseconds) ms" } else { Fail "--wake took $($t.TotalSeconds) s" }

# 3. First launch (laptop mode with wake on): opens the guide and registers the wake task
$cfg = Join-Path $env:APPDATA 'com.earlybolt.app'
New-Item -ItemType Directory -Force $cfg | Out-Null
Set-Content "$cfg\settings.json" '{"mode":"laptop","wake_enabled":true}'
Start-Process $exe
Start-Sleep 12
Shot '1-guide'

$task = (schtasks /Query /TN 'Earlybolt\Wake' /XML 2>&1 | Out-String)
Set-Content "$out\wake-task.xml" $task
if ($task -match [regex]::Escape("$exe") -and $task -match '--wake') { Pass 'wake task runs earlybolt.exe --wake' } else { Fail "wake task: $task" }
schtasks /Run /TN 'Earlybolt\Wake' | Out-Null
Start-Sleep 4

# 4. Launching again toggles the panel; it must sit inside the work area
Start-Process $exe
Start-Sleep 4
Shot '2-panel'
$area = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
Write-Host "work area: $area"
$wins = [Win]::Visible()
Set-Content "$out\windows.txt" $wins
$panel = $wins | Where-Object { ($_ -split '\|')[2] -eq 'Earlybolt' } | Select-Object -First 1
if (-not $panel) { Fail 'panel not visible after second launch' } else {
  $r = (($panel -split '\|')[3] -split ',') | ForEach-Object { [int]$_ }
  Write-Host "panel: $($r -join ',')"
  if ($r[0] -ge $area.Left -and $r[1] -ge $area.Top -and $r[2] -le $area.Right -and $r[3] -le $area.Bottom) { Pass 'panel inside the work area' } else { Fail "panel $($r -join ',') outside work area $area" }
}

# 5. Settings view
Start-Process $exe -ArgumentList '--settings'
Start-Sleep 4
Shot '3-settings'

# 6. Let the background usage refresh run once more, then check for console windows
Start-Sleep 20
Stop-Process -Id $watch.Id
$consoles = if (Test-Path $watcher) { Get-Content $watcher } else { @() }
if ($consoles.Count -eq 0) { Pass 'no console window appeared' } else { Fail "console windows appeared:`n$($consoles -join "`n")" }

Get-Process earlybolt -ErrorAction SilentlyContinue | Stop-Process
$log = Join-Path $env:LOCALAPPDATA 'com.earlybolt.app\logs\ping.log'
if (Test-Path $log) { Copy-Item $log $out }

if ($failures.Count) { Write-Host "`n$($failures.Count) failure(s)"; exit 1 }
Write-Host "`nall checks passed"
