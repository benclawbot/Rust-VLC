#ifndef AppVersion
#define AppVersion "0.1.0"
#endif
#ifndef SourceDir
#define SourceDir "dist\\package"
#endif
#ifndef OutputDir
#define OutputDir "dist"
#endif

[Setup]
AppId={{C5BAE6C8-7070-4EE7-A0A1-58C9859600A1}
AppName=Rust-VLC
AppVersion={#AppVersion}
AppVerName=Rust-VLC {#AppVersion}
DefaultDirName={autopf}\Rust-VLC
DefaultGroupName=Rust-VLC
UninstallDisplayName=Rust-VLC
SetupIconFile={#SourceDir}\rust-vlc.ico
UninstallDisplayIcon={app}\rust-vlc.ico
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64
PrivilegesRequired=admin
OutputDir={#OutputDir}
OutputBaseFilename=rust-vlc-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
Uninstallable=yes

[Files]
Source: "{#SourceDir}\rust-vlc.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\BUILD-INFO.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README-runtime.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\VLC-THIRD-PARTY.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\Inter-OFL.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\rust-vlc.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\VLC\*"; DestDir: "{app}\VLC"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\Rust-VLC"; Filename: "{app}\rust-vlc.exe"; IconFilename: "{app}\rust-vlc.ico"
Name: "{commondesktop}\Rust-VLC"; Filename: "{app}\rust-vlc.exe"; IconFilename: "{app}\rust-vlc.ico"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"
