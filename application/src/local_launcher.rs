//! Explicit, one-time authorization for this user's local scanner launcher.
//! The task has no trigger and can execute only this installed application.
#[cfg(windows)]
pub fn install(destination: Option<&std::path::Path>) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::os::windows::process::CommandExt;
    yas::utils::ensure_admin()?;
    let source = std::env::current_exe()?;
    let executable = destination.unwrap_or(&source).to_path_buf();
    anyhow::ensure!(
        executable.is_absolute() && executable.file_name() == source.file_name(),
        "the launcher destination must be the absolute canonical executable path"
    );
    let directory = source
        .parent()
        .context("the application has no directory")?;
    let script = r#"
$ErrorActionPreference='Stop'
$identity=[Security.Principal.WindowsIdentity]::GetCurrent()
$sid=$identity.User.Value
$taskName='GGScannerOCR-Local-'+$sid
$exe=$env:GGSCANNER_AUTHORIZED_EXE
$root=[IO.Path]::GetDirectoryName($exe)
$source=$env:GGSCANNER_SOURCE_EXE
$action=New-ScheduledTaskAction -Execute $exe -Argument '--agent-mode' -WorkingDirectory $root
$principal=New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Highest
$settings=New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
$existing=Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($existing -and ($existing.Actions.Count -ne 1 -or $existing.Actions[0].Execute -ne $exe)) { throw 'An existing launcher points to a different application; it was not replaced.' }
Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings -Description 'User-authorized, on-demand local GGScannerOCR launcher. No automatic trigger or stored password.' -Force | Out-Null
$data=Join-Path $root 'data'
[IO.Directory]::CreateDirectory($data) | Out-Null
@{schemaVersion=1;taskName=$taskName;executable=$exe;userSid=$sid;registeredAt=[DateTime]::UtcNow.ToString('o')} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $data 'local-launcher.json') -Encoding UTF8
$shell=New-Object -ComObject WScript.Shell
$shortcut=$shell.CreateShortcut((Join-Path $root 'GGScannerOCR.lnk'))
$shortcut.TargetPath=Join-Path $env:WINDIR 'System32\schtasks.exe'
$shortcut.Arguments='/Run /TN "'+$taskName+'"'
$shortcut.IconLocation=$exe+',0'
$shortcut.WorkingDirectory=$root
$shortcut.WindowStyle=7
$shortcut.Save()
if ($source -ne $exe) {
    Get-CimInstance Win32_Process -Filter "Name='GGScannerOCR.exe' OR Name='GGScanner.exe'" | Where-Object { $_.ExecutablePath -eq $exe } | ForEach-Object {
        Stop-Process -Id $_.ProcessId -Force -ErrorAction Stop
        Wait-Process -Id $_.ProcessId -Timeout 10 -ErrorAction SilentlyContinue
    }
}
Write-Output $taskName
"#;
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("GGSCANNER_AUTHORIZED_EXE", &executable)
        .env("GGSCANNER_SOURCE_EXE", &source)
        .current_dir(directory)
        .creation_flags(0x08000000)
        .output()
        .context("could not register the local application launcher")?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }
    // Authorization is already durable even if replacing a locked image needs
    // another attempt. Reuse the normal updater's staging and rollback logic.
    if source != executable {
        genshin_scanner::updater::install_local_executable(&source, &executable)?;
    }
    let task_name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    anyhow::ensure!(
        task_name.starts_with("GGScannerOCR-Local-S-1-"),
        "unexpected launcher registration result"
    );
    let started = std::process::Command::new("schtasks.exe")
        .args(["/Run", "/TN", &task_name])
        .creation_flags(0x08000000)
        .output()
        .context("could not start the authorized local launcher")?;
    anyhow::ensure!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    Ok(())
}
