; Inno Setup Script for Codex OmniBridge
; Creates a native Windows Setup.exe with cleanly registered Uninstaller

#define MyAppName "Codex OmniBridge"
#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif
#ifndef MyProjectRoot
  #define MyProjectRoot "..\.."
#endif
#define MyAppPublisher "Codex MultiProvider Contributors"
#define MyAppURL "https://github.com"
#define MyAppExeName "codex-mp.exe"

[Setup]
AppId={{D64970E6-2007-4C0A-90D1-657D96DBCB61}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={localappdata}\Programs\CodexOmniBridge
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=dist
OutputBaseFilename=codex-omnibridge-windows-setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\{#MyAppExeName}

[Languages]
; Keep the installer self-contained: the GitHub-hosted Inno Setup package does not
; consistently include optional language packs such as ChineseSimplified.isl.
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "startupicon"; Description: "开机自动在系统托盘启动"; GroupDescription: "自启动选项:"; Flags: unchecked

[Files]
Source: "{#MyProjectRoot}\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#MyProjectRoot}\assets\icon.png"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#MyProjectRoot}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#MyProjectRoot}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#MyProjectRoot}\installer\install-windows.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#MyProjectRoot}\installer\uninstall-windows.ps1"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Parameters: "web start"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Parameters: "web start"; Tasks: desktopicon
Name: "{userstartup}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Parameters: "web start"; Tasks: startupicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Parameters: "web start --open"; Description: "立即启动 Codex OmniBridge 并打开网页面板"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; 卸载前首先调用 codex-mp uninstall 安全还原受管的 Codex config.toml，清理 keys 与 catalog，完全零污染
Filename: "{app}\{#MyAppExeName}"; Parameters: "uninstall"; Flags: runhidden waituntilterminated

[Code]
// 在卸载之前，确保正在运行的 codex-mp 进程被安全终止
function InitializeUninstall(): Boolean;
var
  ResultCode: Integer;
begin
  Result := True;
  // 终止运行中的后台实例
  Exec('taskkill.exe', '/F /IM ' + '{#MyAppExeName}', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;
