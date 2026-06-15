; WinHint installer (Inno Setup 6).
;
; Per-user install (no admin needed): installs to %LOCALAPPDATA%\Programs\WinHint.
; A per-user, user-writable location is deliberate — WinHint's WebView2 overlay
; writes a "winhint.exe.WebView2" data folder next to the exe, which would fail
; under read-only Program Files.
;
; ── How to build the installer ────────────────────────────────────────────────
;   1. Build the release binary:   cd winhint && cargo build --release
;   2. Install Inno Setup 6.3+:     https://jrsoftware.org/isdl.php
;      (6.3+ is required for the `x64compatible` architecture setting below)
;   3. Compile this script:
;        "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\winhint.iss
;      (or open this file in the Inno Setup IDE and press F9)
;   Output:  installer\Output\WinHint-Setup-<version>.exe
;
; Signing is intentionally deferred — the produced installer is unsigned, so
; SmartScreen will warn on first run until a code-signing certificate is wired in.

#define AppName "WinHint"
#define AppVersion "0.1.0"
#define AppPublisher "Phillip L. Bronson"
#define AppExe "winhint.exe"
#define AppUrl "https://github.com/catherinezetadrones-stack/keeb-wind-nav"

[Setup]
; A stable, unique AppId — do not change it across versions (ties upgrades
; together and identifies the app for uninstall).
AppId={{8F3A9C7E-1B4D-4E2A-9C6F-2A7B5E0D1C34}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}
DefaultDirName={localappdata}\Programs\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=Output
OutputBaseFilename=WinHint-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
LicenseFile=..\LICENSE
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
; Detect a running instance (matches the daemon's single-instance mutex,
; created as "Local\WinHint_SingleInstance" in main.rs) so install/uninstall
; can prompt to close it instead of failing on a locked exe.
AppMutex=WinHint_SingleInstance
CloseApplications=yes
; Installer .exe file metadata (shown in its Properties / by SmartScreen).
VersionInfoVersion={#AppVersion}
VersionInfoCompany={#AppPublisher}
VersionInfoProductName={#AppName}
VersionInfoDescription={#AppName} Setup

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "startup"; Description: "Start {#AppName} automatically when I sign in"; GroupDescription: "Startup:"

[Files]
Source: "..\winhint\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"

[Registry]
; Run-at-sign-in (per the optional Startup task). Removed on uninstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; \
    ValueType: string; ValueName: "{#AppName}"; ValueData: """{app}\{#AppExe}"""; \
    Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{app}\{#AppExe}"; Description: "Launch {#AppName} now"; \
    Flags: nowait postinstall skipifsilent

[Code]
{ The Evergreen WebView2 Runtime registers its version under this Edge Update
  client GUID. Check all three locations (per-machine x64, per-machine, per-user). }
function WebView2Installed(): Boolean;
var
  Pv: String;
begin
  Result :=
    RegQueryStringValue(HKLM, 'SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', Pv) or
    RegQueryStringValue(HKLM, 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', Pv) or
    RegQueryStringValue(HKCU, 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}', 'pv', Pv);
end;

function InitializeSetup(): Boolean;
begin
  Result := True;
  if not WebView2Installed() then
  begin
    if MsgBox('The Microsoft Edge WebView2 Runtime was not detected.' + #13#10 +
              'WinHint needs it to draw its overlay. It is preinstalled on Windows 11;' + #13#10 +
              'on Windows 10 install it from:' + #13#10 +
              'https://developer.microsoft.com/microsoft-edge/webview2/' + #13#10#13#10 +
              'Continue installing WinHint anyway?', mbConfirmation, MB_YESNO) = IDNO then
      Result := False;
  end;
end;
