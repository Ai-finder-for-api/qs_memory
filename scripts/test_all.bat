@echo off
setlocal
set "ROOT=%~dp0.."
cd /d "%ROOT%"
set "CARGO=cargo"
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"
rem Stop any test binary left running by an earlier run (it locks the output file).
taskkill /F /IM qs_memory-*.exe >nul 2>&1
"%CARGO%" test || exit /b 1
"%CARGO%" build --examples || exit /b 1
"%CARGO%" build --release || exit /b 1
call tests\c\build_c_abi.bat || exit /b 1
echo All checks passed.
