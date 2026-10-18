@echo off
set "PATH=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\bin\HostX64\x64;%PATH%"
set "INCLUDE=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\include;C:\Program Files (x86)\Windows Kits\10\Include\10.0.28000.0\ucrt;C:\Program Files (x86)\Windows Kits\10\Include\10.0.28000.0\shared;C:\Program Files (x86)\Windows Kits\10\Include\10.0.28000.0\um"
set "LIB=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.44.35207\lib\x64;C:\Program Files (x86)\Windows Kits\10\Lib\10.0.28000.0\ucrt\x64;C:\Program Files (x86)\Windows Kits\10\Lib\10.0.28000.0\um\x64"
cd /d C:\Users\Yash\Desktop\quantum-suite\core\qs_memory
cl /nologo /W3 /I include\qs_memory tests\c\test_abi.c target\release\qs_memory.dll.lib /Fe:tests\c\test_abi.exe /Fotests\c\ /link /NOLOGO || exit /b 1
cl /nologo /W3 /I include\qs_memory tests\c\test_heaps.c target\release\qs_memory.dll.lib /Fe:tests\c\test_heaps.exe /Fotests\c\ /link /NOLOGO || exit /b 1
cl /nologo /EHsc /W3 /std:c++17 /I include\qs_memory tests\c\test_cpp.cpp target\release\qs_memory.dll.lib /Fe:tests\c\test_cpp.exe /Fotests\c\ /link /NOLOGO || exit /b 1
copy /Y target\release\qs_memory.dll tests\c\ >nul || exit /b 1
tests\c\test_abi.exe || exit /b 1
tests\c\test_heaps.exe || exit /b 1
tests\c\test_cpp.exe || exit /b 1
echo All C and C++ checks passed.
