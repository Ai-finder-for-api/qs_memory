@echo off
setlocal
cd /d "%~dp0.."
set "CARGO=cargo"
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"
"%CARGO%" build --release || exit /b 1
echo Built target\release\qs_memory.dll
