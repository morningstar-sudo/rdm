#ifndef RdmVersion
  #define RdmVersion "0.1.0"
#endif

#define AppName "RDM"
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
CloseApplicationsFilter=rdm.exe,rdm-host.exe
CloseApplications=yes
RestartApplications=no
ArchiveExtraction=full

[Files]
Source: "{tmp}\RDM-Windows-x64.zip"; DestDir: "{app}"; Flags: external extractarchive recursesubdirs ignoreversion

[Icons]
Name: "{autoprograms}\RDM"; Filename: "{app}\rdm.exe"

[Registry]
Root: HKCU; Subkey: "Software\Classes\rdm"; ValueType: string; ValueName: ""; ValueData: "URL:RDM Protocol"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\rdm"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\rdm\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\rdm.exe"" --capture-url ""%1"""
Root: HKCU; Subkey: "Software\Microsoft\Edge\NativeMessagingHosts\com.rdm.download"; ValueType: string; ValueName: ""; ValueData: "{localappdata}\RDM\com.rdm.download.json"; Flags: uninsdeletekey

[Run]
Filename: "{app}\rdm.exe"; Description: "Launch RDM and finish Edge setup"; Flags: postinstall nowait skipifsilent

[Code]
const
  ReleaseBaseUrl = 'https://github.com/morningstar-sudo/rdm/releases/latest/download/';
  PackageName = 'RDM-Windows-x64.zip';
  ChecksumName = 'RDM-Windows-x64.zip.sha256';

var
  DownloadPage: TDownloadWizardPage;

procedure InitializeWizard;
begin
  DownloadPage := CreateDownloadPage(
    'Downloading RDM',
    'The installer downloads the latest RDM package and verifies its SHA-256 checksum.',
    nil);
  DownloadPage.ShowBaseNameInsteadOfUrl := True;
end;

function IsValidSha256(const Hash: String): Boolean;
var
  I: Integer;
  C: Char;
begin
  Result := (Length(Hash) = 64);
  if not Result then
    Exit;

  for I := 1 to Length(Hash) do begin
    C := Hash[I];
    if not (((C >= '0') and (C <= '9')) or
            ((C >= 'a') and (C <= 'f')) or
            ((C >= 'A') and (C <= 'F'))) then begin
      Result := False;
      Exit;
    end;
  end;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  ChecksumFile: String;
  ChecksumText: AnsiString;
  ExpectedHash: String;
  ErrorText: String;
begin
  Result := True;
  if CurPageID <> wpReady then
    Exit;

  try
    DownloadTemporaryFile(
      ReleaseBaseUrl + ChecksumName, ChecksumName, '', nil);
    ChecksumFile := ExpandConstant('{tmp}\') + ChecksumName;
    if not LoadStringFromFile(ChecksumFile, ChecksumText) then
      RaiseException('Could not read the downloaded SHA-256 checksum.');
    ExpectedHash := Lowercase(Trim(Copy(ChecksumText, 1, 64)));
    if not IsValidSha256(ExpectedHash) then
      RaiseException('The release checksum file is invalid.');

    DownloadPage.Clear;
    DownloadPage.Add(
      ReleaseBaseUrl + PackageName, PackageName, ExpectedHash);
    DownloadPage.Show;
    try
      DownloadPage.Download;
    finally
      DownloadPage.Hide;
    end;
  except
    if DownloadPage.AbortedByUser then begin
      Log('RDM package download was cancelled by the user.');
    end else begin
      ErrorText := GetExceptionMessage;
      SuppressibleMsgBox(
        'Could not download or verify the latest RDM package.' + #13#10 + #13#10 +
        ErrorText + #13#10 + #13#10 +
        'Check your Internet connection and try again.',
        mbCriticalError, MB_OK, IDOK);
    end;
    Result := False;
  end;
end;

[UninstallDelete]
Type: filesandordirs; Name: "{localappdata}\RDM\Rdm.Extension"
Type: files; Name: "{localappdata}\RDM\com.rdm.download.json"
Type: files; Name: "{localappdata}\RDM\edge-integration-v1"
Type: files; Name: "{localappdata}\RDM\protocol-registered-v1"
