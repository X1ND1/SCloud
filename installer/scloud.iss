; SCloud installer (Inno Setup 6). Built by build-installer.bat.
; Installs into the user profile, so no administrator rights are needed.

#define MyAppName "SCloud"
#ifndef MyAppVersion
  #define MyAppVersion "0.2.0"
#endif

[Setup]
AppId={{8C1B6D52-3F7E-4A8B-9E21-5C0D4E7F1A93}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher=SCloud
DefaultDirName={localappdata}\Programs\SCloud
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=SCloud-Setup-{#MyAppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
SetupIconFile=..\assets\scloud.ico
UninstallDisplayIcon={app}\scloud.exe
UninstallDisplayName={#MyAppName}
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "ukrainian"; MessagesFile: "compiler:Languages\Ukrainian.isl"

[CustomMessages]
english.TypeFull=Full installation
english.TypeCompact=Without MP3 downloads (smaller)
english.CompMp3=Download tracks as MP3 (ffmpeg)
russian.TypeFull=Полная установка
russian.TypeCompact=Без скачивания MP3 (меньше места)
russian.CompMp3=Скачивание треков в MP3 (ffmpeg)
ukrainian.TypeFull=Повне встановлення
ukrainian.TypeCompact=Без завантаження MP3 (менше місця)
ukrainian.CompMp3=Завантаження треків у MP3 (ffmpeg)

[Types]
Name: "full"; Description: "{cm:TypeFull}"
Name: "compact"; Description: "{cm:TypeCompact}"

[Components]
Name: "main"; Description: "SCloud"; Types: full compact; Flags: fixed
Name: "mp3"; Description: "{cm:CompMp3}"; Types: full

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\app\scloud.exe"; DestDir: "{app}"; Components: main; Flags: ignoreversion
Source: "..\app\libmpv-2.dll"; DestDir: "{app}"; Components: main; Flags: ignoreversion
Source: "..\app\fonts\*"; DestDir: "{app}\fonts"; Components: main; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\app\ffmpeg.exe"; DestDir: "{app}"; Components: mp3; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\SCloud"; Filename: "{app}\scloud.exe"
Name: "{autodesktop}\SCloud"; Filename: "{app}\scloud.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\scloud.exe"; Description: "{cm:LaunchProgram,SCloud}"; Flags: nowait postinstall skipifsilent

; Likes, history and settings live in AppData and Documents\SCloud, so they survive an uninstall.
