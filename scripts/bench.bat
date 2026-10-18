@echo off
setlocal
cd /d "%~dp0.."
set "CARGO=cargo"
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"
"%CARGO%" bench --bench allocators || exit /b 1
"%CARGO%" bench --bench containers || exit /b 1
