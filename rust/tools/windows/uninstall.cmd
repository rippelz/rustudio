@echo off
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1" -Uninstall -Prefix "%~dp0." %*
if errorlevel 1 (
  echo Uninstallation failed. Close rustudio before retrying.
  pause
  exit /b 1
)
