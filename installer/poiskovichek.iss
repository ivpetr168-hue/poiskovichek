; Скрипт Inno Setup для сборки установщика программы «ПоисКовичеК»
; Кодировка: UTF-8 с BOM

#define MyAppName "ПоисКовичеК"
#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif
#define MyAppPublisher "ПоисКовичеК"
#define MyAppExeName "Poiskovichek.exe"

[Setup]
AppId={{8B84C8AE-635E-4B02-B83F-3277FE38CE76}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={localappdata}\Programs\{#MyAppName}
DefaultGroupName={#MyAppName}
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=ПоисКовичеК-установка
SetupIconFile=..\assets\poiskovichek.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
LicenseFile=..\LICENSE
; AppMutex не используется: в тихом режиме он мог отказать сразу, пока старая программа ещё
; закрывается. Мьютекс «ПоисКовичеК-запущен» проверяет и ждёт InitializeSetup (см. [Code]).
CloseApplications=yes
CloseApplicationsFilter=*.exe
UsePreviousAppDir=yes

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\assets\morph.fst"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\THIRD_PARTY.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent
; После тихой установки (самообновление) программа запускается сама
Filename: "{app}\{#MyAppExeName}"; Flags: nowait skipifnotsilent

[Code]
// Программа держит мьютекс, пока работает. Тихая установка (самообновление) может начаться,
// пока старая копия ещё закрывается, поэтому ждём до 30 с.
function InitializeSetup(): Boolean;
var
  I: Integer;
begin
  if WizardSilent then
  begin
    for I := 1 to 60 do
    begin
      if not CheckForMutexes('ПоисКовичеК-запущен') then
        Break;
      Sleep(500);
    end;
  end;
  Result := True;
  if CheckForMutexes('ПоисКовичеК-запущен') and not WizardSilent then
  begin
    MsgBox(FmtMessage(SetupMessage(msgSetupAppRunningError), ['{#MyAppName}']), mbError, MB_OK);
    Result := False;
  end;
end;

// Удаление данных и автозапуска при удалении программы
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: String;
  ConfigDir: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    // Запись автозапуска в HKCU\Software\Microsoft\Windows\CurrentVersion\Run удаляется всегда
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#MyAppName}');

    // Вопрос пользователю об удалении базы поиска и настроек (по умолчанию «Нет»)
    if MsgBox('Удалить также базу поиска и настройки?', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DataDir := ExpandConstant('{localappdata}\{#MyAppName}');
      ConfigDir := ExpandConstant('{userappdata}\{#MyAppName}');
      if DirExists(DataDir) then
        DelTree(DataDir, True, True, True);
      if DirExists(ConfigDir) then
        DelTree(ConfigDir, True, True, True);
    end;
  end;
end;
