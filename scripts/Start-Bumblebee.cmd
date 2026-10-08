@echo off
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0Start-Bumblebee.ps1" %*
set "BUMBLEBEE_EXIT=%ERRORLEVEL%"
if not "%BUMBLEBEE_EXIT%"=="0" pause
exit /b %BUMBLEBEE_EXIT%
