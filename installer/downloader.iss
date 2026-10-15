; Inno Setup script for Downloader (Windows x64).
; Build:  ISCC.exe /DAppVersion=0.1.0 /DSrcDir=..\target\release installer\downloader.iss
; Requires installer\ChineseTraditional.isl (fetched by scripts\build-installer.ps1).

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SrcDir
  #define SrcDir "..\target\release"
#endif
#define AppName "Downloader"
#define AppExe "downloader.exe"

[Setup]
; Keep AppId stable across versions so new installers upgrade in place.
AppId={{6C1F7E52-0B7A-4C2E-9E3B-5B8D2A41D7C4}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Ke Sheng Da
AppPublisherURL=https://github.com/stevenke1981/Downloader
AppSupportURL=https://github.com/stevenke1981/Downloader/issues
AppUpdatesURL=https://github.com/stevenke1981/Downloader/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE
OutputDir=..\dist
OutputBaseFilename=downloader-setup-windows-x64
SetupIconFile=..\gui\assets\downloader.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName} {#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=dialog commandline
ChangesEnvironment=yes
CloseApplications=yes
RestartApplications=no
ShowLanguageDialog=auto

[Languages]
Name: "chinesetraditional"; MessagesFile: "ChineseTraditional.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
chinesetraditional.TaskPath=將 dlx 命令列下載工具加入 PATH
chinesetraditional.GroupIntegration=整合：
chinesetraditional.TaskBrowser=註冊 Chrome / Edge 瀏覽器擴充功能的連線元件（Native Messaging）
english.TaskBrowser=Register the Chrome / Edge extension connector (native messaging host)
english.TaskPath=Add the dlx command-line downloader to PATH
english.GroupIntegration=Integration:

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "browser"; Description: "{cm:TaskBrowser}"; GroupDescription: "{cm:GroupIntegration}"
Name: "addtopath"; Description: "{cm:TaskPath}"; GroupDescription: "{cm:GroupIntegration}"; Flags: unchecked

[Files]
Source: "{#SrcDir}\downloader.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\dlx.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\downloader-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\extension\*"; DestDir: "{app}\extension"; Excludes: "EXTENSION_ID"; Flags: ignoreversion recursesubdirs
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\desktop-controls.zh-TW.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\validation-0.4.0.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Parameters: "--register-browser"; Flags: runhidden waituntilterminated; Tasks: browser; StatusMsg: "{cm:TaskBrowser}"
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\{#AppExe}"; Parameters: "--unregister-browser"; Flags: runhidden waituntilterminated; RunOnceId: "UnregisterBrowser"

[Code]
function EnvKeyRoot: Integer;
begin
  if IsAdminInstallMode then Result := HKEY_LOCAL_MACHINE else Result := HKEY_CURRENT_USER;
end;

function EnvKeyPath: String;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

function PathContains(const Paths, Dir: String): Boolean;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') > 0;
end;

procedure AddToPath(const Dir: String);
var Paths: String;
begin
  if not RegQueryStringValue(EnvKeyRoot, EnvKeyPath, 'Path', Paths) then Paths := '';
  if PathContains(Paths, Dir) then exit;
  if (Paths <> '') and (Copy(Paths, Length(Paths), 1) <> ';') then Paths := Paths + ';';
  RegWriteExpandStringValue(EnvKeyRoot, EnvKeyPath, 'Path', Paths + Dir);
end;

procedure RemoveFromPath(const Dir: String);
var Paths, P: String; I: Integer;
begin
  if not RegQueryStringValue(EnvKeyRoot, EnvKeyPath, 'Path', Paths) then exit;
  P := ';' + Paths + ';';
  I := Pos(';' + Uppercase(Dir) + ';', Uppercase(P));
  if I = 0 then exit;
  Delete(P, I, Length(Dir) + 1);
  P := Copy(P, 2, Length(P) - 2);
  RegWriteExpandStringValue(EnvKeyRoot, EnvKeyPath, 'Path', P);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then
    AddToPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
