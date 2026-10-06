#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef BuildDir
  #error BuildDir must point to the GPUI release output directory.
#endif
#ifndef ResourceDir
  #error ResourceDir must point to native-assets/resources.
#endif
#ifndef AppIcon
  #error AppIcon must point to the generated multi-size Explorie ICO.
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif
#ifndef OutputName
  #define OutputName "explorie-windows-x64-setup"
#endif

[Setup]
AppId={{9B959691-9A96-4F78-9B17-90410427BD0F}
AppName=Explorie
AppVersion={#AppVersion}
AppPublisher=Explorie contributors
AppPublisherURL=https://github.com/bildhaus/explorie
AppSupportURL=https://github.com/bildhaus/explorie/issues
AppUpdatesURL=https://github.com/bildhaus/explorie/releases
DefaultDirName={localappdata}\Programs\Explorie
DefaultGroupName=Explorie
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}
SetupIconFile={#AppIcon}
UninstallDisplayIcon={app}\Explorie.exe
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
ChangesAssociations=yes
VersionInfoVersion={#AppVersion}
VersionInfoCompany=Explorie contributors
VersionInfoDescription=Explorie installer
VersionInfoProductName=Explorie
VersionInfoProductVersion={#AppVersion}

[InstallDelete]
Type: files; Name: "{app}\plugins\explorie-plugin-syncthing-*-x86_64-pc-windows-msvc.zip"
Type: files; Name: "{app}\plugins\explorie-plugin-git-*-x86_64-pc-windows-msvc.zip"
Type: files; Name: "{app}\plugins\explorie-plugin-obsidian-*-x86_64-pc-windows-msvc.zip"

[Files]
Source: "{#BuildDir}\explorie-gpui.exe"; DestDir: "{app}"; DestName: "Explorie.exe"; Flags: ignoreversion
Source: "{#BuildDir}\rclone.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\7zip\7z.exe"; DestDir: "{app}\7zip"; Flags: ignoreversion
Source: "{#BuildDir}\7zip\7z.dll"; DestDir: "{app}\7zip"; Flags: ignoreversion
Source: "{#BuildDir}\plugins\explorie-plugin-syncthing-{#AppVersion}-x86_64-pc-windows-msvc.zip"; DestDir: "{app}\plugins"; Flags: ignoreversion
Source: "{#BuildDir}\plugins\explorie-plugin-git-{#AppVersion}-x86_64-pc-windows-msvc.zip"; DestDir: "{app}\plugins"; Flags: ignoreversion
Source: "{#BuildDir}\plugins\explorie-plugin-obsidian-{#AppVersion}-x86_64-pc-windows-msvc.zip"; DestDir: "{app}\plugins"; Flags: ignoreversion
Source: "{#ResourceDir}\7zip-LICENSE.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#ResourceDir}\7zip-COPYING.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#ResourceDir}\7zip-NOTICE.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#BuildDir}\licenses\7z2603-src.tar.xz"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#ResourceDir}\winfsp-2.1.25156.msi"; DestDir: "{app}\installers"; Flags: ignoreversion
Source: "{#ResourceDir}\rclone-COPYING"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#ResourceDir}\winfsp-NOTICE.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#ResourceDir}\pixelarticons-LICENSE.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#ResourceDir}\assimp-LICENSE.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion

[Icons]
Name: "{group}\Explorie"; Filename: "{app}\Explorie.exe"; WorkingDir: "{app}"

[Run]
Filename: "{app}\Explorie.exe"; Description: "Launch Explorie"; Flags: nowait postinstall skipifsilent
Filename: "{app}\Explorie.exe"; Flags: nowait; Check: RelaunchRequested
Filename: "{app}\Explorie.exe"; Parameters: "--cleanup-installer ""{srcexe}"""; Description: "Delete the downloaded installer"; Flags: nowait postinstall skipifsilent runhidden; Check: ManualCleanupOffered
Filename: "{app}\Explorie.exe"; Parameters: "--cleanup-installer ""{srcexe}"""; Flags: nowait runhidden; Check: RelaunchRequested

[UninstallRun]
Filename: "{app}\Explorie.exe"; Parameters: "--unregister-folder-handler"; Flags: runhidden waituntilterminated skipifdoesntexist; RunOnceId: "RestoreFolderHandler"

[Code]
function RelaunchRequested: Boolean;
var
  Index: Integer;
begin
  Result := False;
  for Index := 1 to ParamCount do
    if CompareText(ParamStr(Index), '/RELAUNCHEXPLORIE') = 0 then
    begin
      Result := True;
      Exit;
    end;
end;

function ManualCleanupOffered: Boolean;
begin
  Result := not RelaunchRequested;
end;
