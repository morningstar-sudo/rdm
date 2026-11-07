#ifndef RdmVersion
  #define RdmVersion "0.1.0"
#endif

#define AppName "RDM"
#define AppExeName "rdm.exe"
#define AppId "{{A4D7435D-2AA3-4AB8-A426-6B0718C6D28F}"

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#RdmVersion}
AppPublisher=morningstar-sudo
DefaultDirName={localappdata}\Programs\RDM
DefaultGroupName=RDM
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
OutputDir=..\dist
OutputBaseFilename=RDM-Setup-x64
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\rdm.exe
CloseApplications=yes
RestartApplications=no

[Files]
Source: "..\dist\RDM-Windows-x64\rdm.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\RDM-Windows-x64\Rdm.Extension\*"; DestDir: "{app}\Rdm.Extension"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\RDM"; Filename: "{app}\rdm.exe"

[Registry]
Root: HKCU; Subkey: "Software\Classes\rdm"; ValueType: string; ValueName: ""; ValueData: "URL:RDM Protocol"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\rdm"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\rdm\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\rdm.exe"" --capture-url ""%1"""
Root: HKCU; Subkey: "Software\Microsoft\Edge\NativeMessagingHosts\com.rdm.download"; ValueType: string; ValueName: ""; ValueData: "{localappdata}\RDM\com.rdm.download.json"; Flags: uninsdeletekey

[Run]
Filename: "{app}\rdm.exe"; Description: "Launch RDM and finish Edge setup"; Flags: postinstall nowait skipifsilent

[UninstallDelete]
Type: filesandordirs; Name: "{localappdata}\RDM\Rdm.Extension"
Type: files; Name: "{localappdata}\RDM\com.rdm.download.json"
Type: files; Name: "{localappdata}\RDM\edge-integration-v1"
Type: files; Name: "{localappdata}\RDM\protocol-registered-v1"
