@echo off
REM Initialize Visual Studio developer environment
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat" -arch=x64 -host_arch=x64

REM Add Node.js and Cargo to PATH
set PATH=C:\Program Files\nodejs;%USERPROFILE%\.cargo\bin;%PATH%

REM Launch Tauri dev
cd /d "%~dp0"
call npm run tauri:dev
