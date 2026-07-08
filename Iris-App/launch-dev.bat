@echo off
setlocal
pushd "%~dp0"

echo [Iris Launch] Preparing dependencies and workspace...
call "%~dp0setup-windows.bat" --dev --yes
if errorlevel 1 (
	echo [Iris Launch] Setup failed. Aborting launch.
	popd
	exit /b 1
)

if exist "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat" (
	call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat" -arch=x64 -host_arch=x64 >nul
)

echo [Iris Launch] Starting Tauri development runtime...
call npm run tauri:dev
set "EXIT_CODE=%ERRORLEVEL%"

popd
exit /b %EXIT_CODE%
