@echo off
setlocal
pushd "%~dp0"

echo [Iris Start] Bootstrapping dependencies for frontend dev...
call "%~dp0setup-windows.bat" --dev --yes --skip-buildtools
if errorlevel 1 (
	echo [Iris Start] Setup failed. Aborting.
	popd
	exit /b 1
)

echo [Iris Start] Launching Vite dev server...
call npm run dev
set "EXIT_CODE=%ERRORLEVEL%"

popd
exit /b %EXIT_CODE%
