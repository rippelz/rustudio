@echo off
powershell.exe -NoProfile -STA -ExecutionPolicy Bypass -File "%~dp0install.ps1" -Interactive %*
if errorlevel 1 (
  echo Installation failed. Close rustudio before retrying.
  pause
  exit /b 1
)
