; Process Job Hacker installer.
; Build it with scripts/package.ps1, which passes AppVersion and SourceDir.
#ifndef AppVersion
  #error Define AppVersion, for example /DAppVersion=0.1.0
#endif
#ifndef SourceDir
  #error Define SourceDir as the staged release folder
#endif

; The Windows file version takes numbers only, so a prerelease label is left out.
#if Pos("-", AppVersion) > 0
  #define FileVersion Copy(AppVersion, 1, Pos("-", AppVersion) - 1)
#else
  #define FileVersion AppVersion
#endif

; Must stay stable forever: a different AppId installs a second copy instead of upgrading.
#ifndef AppId
  #define AppId "8F3C2A5E-6B1D-4E9A-9C7F-2D4B6A8E1F30"
#endif
#ifndef AppName
  #define AppName "Process Job Hacker"
#endif
#ifndef MainExe
  #define MainExe "process-job-hacker.exe"
#endif
#ifndef DataDirRoaming
  #define DataDirRoaming "{userappdata}\prostones\ProcessJobHacker"
#endif
#ifndef DataDirLocal
  #define DataDirLocal "{localappdata}\prostones\ProcessJobHacker"
#endif
#define UninstallKey "Software\Microsoft\Windows\CurrentVersion\Uninstall\{" + AppId + "}_is1"

[Setup]
AppId={{{#AppId}}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=prostones
AppPublisherURL=https://github.com/prostones/win-job-limiter
AppSupportURL=https://github.com/prostones/win-job-limiter/issues
AppUpdatesURL=https://github.com/prostones/win-job-limiter/releases
AppCopyright=Copyright (C) 2026 prostones
VersionInfoCompany=prostones
VersionInfoDescription={#AppName} installer
VersionInfoProductName={#AppName}
VersionInfoProductVersion={#FileVersion}
VersionInfoVersion={#FileVersion}
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\{#MainExe}
; Default to a per-user install: no administrator prompt for setup or updates.
; The first dialog still lets the user choose an all-users install.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
DefaultDirName={autopf}\{#AppName}
DisableDirPage=yes
DisableReadyPage=yes
DisableProgramGroupPage=yes
MinVersion=10.0.17763
SetupArchitecture=x64
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern dynamic
SetupIconFile=..\assets\app.ico
RestartApplications=no
SetupLogging=yes
Compression=lzma2/max
SolidCompression=yes
OutputDir=..\dist
OutputBaseFilename=ProcessJobHacker-{#AppVersion}-Setup
ShowLanguageDialog=no
UsePreviousLanguage=no

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "chs"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "cht"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[LangOptions]
cht.LanguageID=$0404

[CustomMessages]
AdditionalIcons=附加任务：
DesktopShortcut=创建桌面快捷方式
LaunchProgram=启动 {#AppName}
ExitFirstTitle=请先退出 {#AppName}
ExitFirstText={#AppName} 正在运行。安装器无法自动关闭它，请右键点击任务栏通知区域中的 {#AppName} 图标，选择“退出”，然后点击“重试”。
OldVersionNotRemoved=无法卸载已安装的旧版本 {#AppName}（代码 %1）。请在 Windows 设置的“应用”中卸载后，重新运行安装程序。
CannotCloseApp=无法自动关闭正在运行的 {#AppName}。请从托盘退出后重新运行安装程序。
DeleteUserDataQuestion=是否同时删除 {#AppName} 的配置、日志和活动 Job 记录？选择“否”会保留这些数据。
en.AdditionalIcons=Additional icons:
en.DesktopShortcut=Create a desktop shortcut
en.LaunchProgram=Launch {#AppName}
en.ExitFirstTitle=Quit {#AppName} first
en.ExitFirstText={#AppName} is running and Setup could not close it automatically. Right-click the {#AppName} icon in the notification area of the taskbar, choose "Exit", then click "Retry".
en.OldVersionNotRemoved=The installed version of {#AppName} could not be uninstalled (code %1). Uninstall it from Apps in Windows Settings, then run Setup again.
en.CannotCloseApp=Could not close the running {#AppName} automatically. Quit it from the notification area, then run Setup again.
en.DeleteUserDataQuestion=Also delete {#AppName} settings, logs and active Job records? Choose "No" to keep them.

[Tasks]
Name: "desktopicon"; Description: "{cm:DesktopShortcut}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "stop-running.ps1"; DestDir: "{tmp}"; Flags: dontcopy noencryption
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#MainExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#MainExe}"; Tasks: desktopicon

; process-job-hacker.exe requires administrator, so launch it through ShellExecute with the
; runas verb. runasoriginaluser uses CreateProcess with the non-elevated token and fails with
; error 740 (ERROR_ELEVATION_REQUIRED).
[Run]
Filename: "{app}\{#MainExe}"; Description: "{cm:LaunchProgram}"; Verb: "runas"; Flags: nowait postinstall skipifsilent shellexec

[UninstallDelete]
Type: filesandordirs; Name: "{app}\temp"

[Code]
var
  DeleteUserData: Boolean;

function HasParam(const Name: String): Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), Name) = 0 then
    begin
      Result := True;
      Exit;
    end;
end;

function AppRunning: Boolean;
begin
  Result := CheckForMutexes('Local\ProcessJobHacker.Instance');
end;

// Signal Local\ProcessJobHacker.Quit without launching the administrator-manifested exe.
// Running the exe itself from a non-elevated installer would fail with error 740.
function RunQuitHelper: Boolean;
var
  Script, Params: String;
  ResultCode: Integer;
begin
  Result := False;
  Script := ExpandConstant('{tmp}\stop-running.ps1');
  Params := '-NoProfile -ExecutionPolicy Bypass -File "' + Script + '"';
  Log('Requesting the running instance to quit without elevation.');
  ResultCode := -1;
  Result := Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'), Params, '',
    SW_HIDE, ewWaitUntilTerminated, ResultCode) and (ResultCode = 0);
  Log('Graceful stop script result: ' + IntToStr(ResultCode));
end;

function WaitForAppExit(TimeoutMs: Integer): Boolean;
var
  Waited: Integer;
begin
  Waited := 0;
  while AppRunning and (Waited < TimeoutMs) do
  begin
    Sleep(100);
    Waited := Waited + 100;
  end;
  Result := not AppRunning;
end;

// Kill only the main and preview processes. The elevated helper reads command lines,
// leaves the --guard process alive, and waits for the journal's guard PID to exit.
// Never use taskkill /IM or /T here: killing the guard could leave limits behind.
function ForceStopRunningApp(Elevate: Boolean): Boolean;
var
  Script, Params: String;
  ResultCode: Integer;
begin
  Result := False;
  if not Elevate then
    Exit;
  Script := ExpandConstant('{tmp}\stop-running.ps1');
  Params := '-NoProfile -ExecutionPolicy Bypass -File "' + Script + '" -Force';
  Log('Requesting elevation to stop the running copy.');
  ResultCode := -1;
  Result := ShellExec('runas', ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    Params, '', SW_HIDE, ewWaitUntilTerminated, ResultCode) and (ResultCode = 0);
  Log('Force-stop script result: ' + IntToStr(ResultCode));
  if Result then
    Result := WaitForAppExit(5000);
end;

function EnsureAppClosed(Elevate: Boolean): Boolean;
begin
  Result := True;
  if not AppRunning then
    Exit;
  ExtractTemporaryFile('stop-running.ps1');
  if RunQuitHelper then
    Exit;
  Log('Graceful quit did not finish; falling back to the elevated stop script.');
  Result := ForceStopRunningApp(Elevate);
end;

function QuotedPath(const Command: String): String;
var
  I: Integer;
begin
  Result := Command;
  if (Length(Command) = 0) or (Command[1] <> '"') then
    Exit;
  Result := '';
  for I := 2 to Length(Command) do
  begin
    if Command[I] = '"' then
      Exit;
    Result := Result + Command[I];
  end;
end;

function InitializeSetup: Boolean;
begin
  Result := EnsureAppClosed(not WizardSilent);
  while (not Result) and (not WizardSilent) do
  begin
    if TaskDialogMsgBox(CustomMessage('ExitFirstTitle'), CustomMessage('ExitFirstText'),
      mbError, MB_RETRYCANCEL, [], 0) = IDCANCEL then
      Exit;
    Result := EnsureAppClosed(True);
  end;
end;

// Run the installed version's uninstaller before copying files, so no stale files remain.
// /UPGRADE keeps user settings, logs and active Job records.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Uninstaller: String;
  ResultCode, Waited: Integer;
begin
  Result := '';
  if not EnsureAppClosed(not WizardSilent) then
  begin
    Result := CustomMessage('CannotCloseApp');
    Exit;
  end;
  if not RegQueryStringValue(HKA, '{#UninstallKey}', 'UninstallString', Uninstaller) then
  begin
    Log('No installed version found.');
    Exit;
  end;
  Uninstaller := QuotedPath(Uninstaller);
  if not FileExists(Uninstaller) then
  begin
    Log('Uninstaller of the installed version is missing: ' + Uninstaller);
    Exit;
  end;
  Log('Removing the installed version: ' + Uninstaller);
  if not Exec(Uninstaller, '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /UPGRADE', '', SW_HIDE,
    ewWaitUntilTerminated, ResultCode) or (ResultCode <> 0) then
  begin
    Log('Uninstall failed with code ' + IntToStr(ResultCode));
    Result := FmtMessage(CustomMessage('OldVersionNotRemoved'), [IntToStr(ResultCode)]);
    Exit;
  end;
  Waited := 0;
  while FileExists(Uninstaller) and (Waited < 10000) do
  begin
    Sleep(200);
    Waited := Waited + 200;
  end;
end;

function InitializeUninstall: Boolean;
begin
  Result := EnsureAppClosed(not UninstallSilent);
  DeleteUserData := False;
  if Result and not UninstallSilent and not HasParam('/UPGRADE') then
    DeleteUserData := MsgBox(CustomMessage('DeleteUserDataQuestion'), mbConfirmation,
      MB_YESNO or MB_DEFBUTTON2) = IDYES;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if (CurUninstallStep = usPostUninstall) and DeleteUserData and not HasParam('/UPGRADE') then
  begin
    DelTree(ExpandConstant('{#DataDirRoaming}'), True, True, True);
    DelTree(ExpandConstant('{#DataDirLocal}'), True, True, True);
  end;
end;
