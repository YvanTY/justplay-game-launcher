; Installer for JustPlay - Game Launcher (Inno Setup 6). Built by packaging\build-release.ps1,
; which stages the files in dist\JustPlay and passes the version.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{25B2475C-CB09-454B-836C-3E241905DD72}
AppName=JustPlay - Game Launcher
AppVersion={#AppVersion}
AppVerName=JustPlay - Game Launcher {#AppVersion}
AppPublisher=YvanTY
AppPublisherURL=https://github.com/YvanTY/justplay-game-launcher
AppSupportURL=https://github.com/YvanTY/justplay-game-launcher/issues
; Installs for the current user: no administrator rights needed (ViGEmBus asks for its own).
PrivilegesRequired=lowest
DefaultDirName={autopf}\JustPlay
DefaultGroupName=JustPlay
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=JustPlay-Setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\JustPlay.exe
UninstallDisplayName=JustPlay - Game Launcher
LicenseFile=..\dist\JustPlay\LICENSE.txt
WizardStyle=modern
Compression=lzma2/ultra
SolidCompression=yes
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"
Name: "startup"; Description: "Start JustPlay when I sign in to Windows (for a couch PC)"; GroupDescription: "Shortcuts:"; Flags: unchecked
Name: "vigembus"; Description: "Install the ViGEmBus driver: lets any controller work in Xbox-only games (recommended)"; GroupDescription: "Controllers:"; Check: not ViGEmBusInstalled

[Files]
Source: "..\dist\JustPlay\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs
Source: "redist\ViGEmBus_1.22.0_x64_x86_arm64.exe"; DestDir: "{tmp}"; Flags: deleteafterinstall; Tasks: vigembus
Source: "ViGEmBus-LICENSE.txt"; DestDir: "{app}"; Tasks: vigembus

[Icons]
Name: "{group}\JustPlay"; Filename: "{app}\JustPlay.exe"
Name: "{autodesktop}\JustPlay"; Filename: "{app}\JustPlay.exe"; Tasks: desktopicon
Name: "{userstartup}\JustPlay"; Filename: "{app}\JustPlay.exe"; Tasks: startup

[Run]
; The driver installer elevates itself (Windows asks for permission).
Filename: "{tmp}\ViGEmBus_1.22.0_x64_x86_arm64.exe"; Parameters: "/passive /norestart"; StatusMsg: "Installing the ViGEmBus controller driver..."; Flags: shellexec waituntilterminated; Verb: "runas"; Tasks: vigembus
Filename: "{app}\JustPlay.exe"; Description: "Start JustPlay"; Flags: nowait postinstall skipifsilent

[Code]
function ViGEmBusInstalled: Boolean;
begin
  Result := RegKeyExists(HKLM, 'SYSTEM\CurrentControlSet\Services\ViGEmBus');
end;
