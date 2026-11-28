; Inno Setup script for the Windows installer, compiled by build-windows-native.sh,
; which passes AppVersion, SourceDir (the portable folder), OutputDir and OutputName.
; Installs for the current user (no administrator rights). Uninstalling leaves the
; settings in %APPDATA%\lavmfx.

[Setup]
; Identifies LAVMFX across versions: never change it.
AppId={{6E0B7A52-3C1D-4F8A-9B27-5D4E1C8A90F3}
AppName=LAVMFX
AppVersion={#AppVersion}
AppPublisher=p0sthuman
AppPublisherURL=https://github.com/p0sthuman/lavmfx
DefaultDirName={autopf}\LAVMFX
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile={#SourceDir}\LICENSE
SetupIconFile=..\app\lavmfx\assets\lavmfx.ico
UninstallDisplayIcon={app}\lavmfx.exe
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\LAVMFX"; Filename: "{app}\lavmfx.exe"
Name: "{autodesktop}\LAVMFX"; Filename: "{app}\lavmfx.exe"; Tasks: desktopicon
