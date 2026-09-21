; TagTiger — Inno Setup installer
; -----------------------------------------------------------------------------
; Builds a single Setup.exe that lets the user choose to install the GUI app,
; the command-line tool, or both. Installs to Program Files, creates Start Menu
; (and optional desktop) shortcuts for the GUI, registers TagTiger as an
; "Open with" handler for .mp4/.m4v, and — when the CLI is selected — adds its
; folder to the per-user PATH. A matching uninstaller reverses all of it.
;
; Invoked by CI as:
;   iscc /DAppVersion=X.Y.Z /DArch=<x64|arm64> /DSourceDir=<dir> tagtiger.iss
; where <dir> contains tagtiger-gui.exe and tagtiger.exe.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef Arch
  #define Arch "x64"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\target\release"
#endif

#define AppName "TagTiger"
#define AppPublisher "Glowing Cat Software"
#define AppURL "https://glowingcat.com/TagTiger.html"
#define GuiExe "tagtiger-gui.exe"
#define CliExe "tagtiger.exe"

[Setup]
AppId={{D501CC2E-7C20-404A-BFF5-68A7FAE3F36D}}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
; Per-user install by default (no admin needed); the user may elect an
; all-users install, which elevates.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
SetupIconFile=..\..\gui\src\resources\app_icon.ico
UninstallDisplayIcon={app}\{#GuiExe}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
OutputDir=.
OutputBaseFilename=TagTiger-{#Arch}-Setup
#if Arch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Types]
Name: "full"; Description: "GUI application and command-line tool"
Name: "gui"; Description: "GUI application only"
Name: "cli"; Description: "Command-line tool only"
Name: "custom"; Description: "Custom"; Flags: iscustom

[Components]
Name: "gui"; Description: "TagTiger GUI (metadata editor)"; Types: full gui custom
Name: "cli"; Description: "TagTiger command-line tool (tagtiger)"; Types: full cli custom

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; \
  GroupDescription: "Additional shortcuts:"; Components: gui; Flags: unchecked
Name: "addtopath"; Description: "Add the command-line tool to your PATH"; \
  GroupDescription: "Command-line tool:"; Components: cli

[Files]
Source: "{#SourceDir}\{#GuiExe}"; DestDir: "{app}"; Components: gui; Flags: ignoreversion
Source: "{#SourceDir}\{#CliExe}"; DestDir: "{app}"; Components: cli; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#GuiExe}"; Components: gui
Name: "{group}\Uninstall {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#GuiExe}"; \
  Components: gui; Tasks: desktopicon

[Registry]
; --- File associations for the GUI: add to "Open with" without hijacking the
;     user's default handler. Written under the install scope's Classes root
;     ({autopf} install -> HKLM; per-user -> HKCU), removed on uninstall. ---
Root: HKA; Subkey: "Software\Classes\TagTiger.Movie"; \
  ValueType: string; ValueName: ""; ValueData: "MPEG-4 Movie"; \
  Components: gui; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\TagTiger.Movie\DefaultIcon"; \
  ValueType: string; ValueName: ""; ValueData: "{app}\{#GuiExe},0"; \
  Components: gui; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\TagTiger.Movie\shell\open\command"; \
  ValueType: string; ValueName: ""; ValueData: """{app}\{#GuiExe}"" ""%1"""; \
  Components: gui; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\.mp4\OpenWithProgids"; \
  ValueType: none; ValueName: "TagTiger.Movie"; \
  Components: gui; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.m4v\OpenWithProgids"; \
  ValueType: none; ValueName: "TagTiger.Movie"; \
  Components: gui; Flags: uninsdeletevalue

[Run]
Description: "Launch {#AppName}"; Filename: "{app}\{#GuiExe}"; \
  Components: gui; Flags: nowait postinstall skipifsilent

; -----------------------------------------------------------------------------
; PATH management for the CLI (per-user PATH under HKCU\Environment). Done in
; code so we can append/remove idempotently without clobbering existing entries.
; -----------------------------------------------------------------------------
[Code]
const
  EnvKey = 'Environment';

function PathListContains(const Paths, Dir: string): Boolean;
var
  Hay, Needle: string;
begin
  Hay := ';' + Lowercase(Paths) + ';';
  Needle := ';' + Lowercase(Dir) + ';';
  Result := Pos(Needle, Hay) > 0;
end;

procedure AddToUserPath(const Dir: string);
var
  Cur: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Cur) then
    Cur := '';
  if PathListContains(Cur, Dir) then
    exit;
  if (Cur <> '') and (Cur[Length(Cur)] <> ';') then
    Cur := Cur + ';';
  Cur := Cur + Dir;
  RegWriteExpandStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Cur);
end;

procedure RemoveFromUserPath(const Dir: string);
var
  Cur, Rebuilt, Item: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Cur) then
    exit;
  Rebuilt := '';
  Cur := Cur + ';';
  repeat
    P := Pos(';', Cur);
    Item := Copy(Cur, 1, P - 1);
    Delete(Cur, 1, P);
    if (Item <> '') and (Lowercase(Item) <> Lowercase(Dir)) then
    begin
      if Rebuilt <> '' then Rebuilt := Rebuilt + ';';
      Rebuilt := Rebuilt + Item;
    end;
  until Cur = '';
  RegWriteExpandStringValue(HKEY_CURRENT_USER, EnvKey, 'Path', Rebuilt);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    // Only touch PATH when the CLI was installed and the task was checked.
    if WizardIsComponentSelected('cli') and WizardIsTaskSelected('addtopath') then
      AddToUserPath(ExpandConstant('{app}'));
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RemoveFromUserPath(ExpandConstant('{app}'));
end;
