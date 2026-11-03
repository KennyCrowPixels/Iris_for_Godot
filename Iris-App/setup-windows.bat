@echo off
setlocal EnableExtensions EnableDelayedExpansion
pushd "%~dp0"

set "MODE=dev"
set "AUTO_YES=0"
set "INCLUDE_BUILD_TOOLS=1"
set "INSTALL_MODELS=0"

:parse_args
if "%~1"=="" goto args_done
if /I "%~1"=="--dev" (
  set "MODE=dev"
) else if /I "%~1"=="--consumer" (
  set "MODE=consumer"
) else if /I "%~1"=="--yes" (
  set "AUTO_YES=1"
) else if /I "%~1"=="--skip-buildtools" (
  set "INCLUDE_BUILD_TOOLS=0"
) else if /I "%~1"=="--with-models" (
  set "INSTALL_MODELS=1"
)
shift
goto parse_args

:args_done
call :step 2 "Starting Windows bootstrap (%MODE% mode)..."

call :require_winget || goto failed

call :step 10 "Checking Git..."
call :ensure_command "git" "Git.Git" "Git for Windows" || goto failed

call :step 20 "Checking Node.js..."
call :ensure_command "node" "OpenJS.NodeJS.LTS" "Node.js LTS" || goto failed

call :step 30 "Checking Rust toolchain..."
call :ensure_command "cargo" "Rustlang.Rustup" "Rust (rustup + cargo)" || goto failed

call :step 38 "Checking Ollama..."
call :ensure_command "ollama" "Ollama.Ollama" "Ollama" || goto failed

if "%INCLUDE_BUILD_TOOLS%"=="1" (
  call :step 45 "Checking C++ Build Tools..."
  call :ensure_build_tools || goto failed
)

call :step 58 "Refreshing PATH for this terminal..."
if exist "%ProgramFiles%\nodejs" set "PATH=%ProgramFiles%\nodejs;%PATH%"
if exist "%USERPROFILE%\.cargo\bin" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
if exist "%LOCALAPPDATA%\Programs\Ollama" set "PATH=%LOCALAPPDATA%\Programs\Ollama;%PATH%"

call :step 64 "Ensuring stable Rust toolchain..."
if exist "%USERPROFILE%\.cargo\bin\rustup.exe" (
  call rustup default stable >nul 2>nul
)

call :step 72 "Checking and installing npm dependencies..."
call npm install --include=dev --no-audit --no-fund
if not "%ERRORLEVEL%"=="0" (
  echo [Iris Setup] npm install failed.
  echo [Iris Setup] For EPERM or EBUSY, close this workspace's Iris/Vite processes and retry.
  goto failed
)

call :step 84 "Validating TypeScript setup..."
call npm run check
if not "%ERRORLEVEL%"=="0" (
  echo [Iris Setup] TypeScript check failed.
  goto failed
)

if "%INSTALL_MODELS%"=="1" (
  call node scripts\setup-models.mjs --profile balanced
  if errorlevel 1 goto failed
)

if "%MODE%"=="consumer" (
  call :step 92 "Consumer mode ready. Use build-release.bat to create installers."
) else (
  call :step 92 "Developer mode ready. Use launch-dev.bat to run Iris Desktop."
)

call :step 100 "Bootstrap completed successfully."
echo.
echo [Iris Setup] Completed. Next steps:
if "%MODE%"=="consumer" (
  echo   1. Run build-release.bat
  echo   2. Share the generated installer from the builds folder
) else (
  echo   1. Run launch-dev.bat
  echo   2. Wait for Tauri + Vite to finish startup
)
popd
exit /b 0

:failed
echo.
echo [Iris Setup] Bootstrap failed.
echo [Iris Setup] Resolve the error above, then rerun this script.
popd
exit /b 1

:step
set "PCT=%~1"
set "MSG=%~2"
echo [Iris Setup] [%PCT%%%] %MSG%
exit /b 0

:require_winget
where winget >nul 2>nul
if errorlevel 1 (
  echo [Iris Setup] winget was not found. Install/update App Installer from Microsoft Store, then rerun.
  exit /b 1
)
exit /b 0

:ensure_command
set "CMD_NAME=%~1"
set "PKG_ID=%~2"
set "DISPLAY=%~3"
where %CMD_NAME% >nul 2>nul
if not errorlevel 1 (
  echo [Iris Setup] !DISPLAY! is already installed.
  exit /b 0
)

echo [Iris Setup] !DISPLAY! missing. Installing via winget...
if "%AUTO_YES%"=="1" (
  winget install --id !PKG_ID! -e --accept-package-agreements --accept-source-agreements
) else (
  winget install --id !PKG_ID! -e --accept-package-agreements --accept-source-agreements
)
if errorlevel 1 (
  echo [Iris Setup] Failed to install !DISPLAY! (!PKG_ID!).
  exit /b 1
)
exit /b 0

:ensure_build_tools
set "VSDEVCMD_X86=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat"
set "VSDEVCMD_X64=C:\Program Files\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat"
where cl >nul 2>nul
if not errorlevel 1 (
  echo [Iris Setup] Visual C++ Build Tools already available.
  exit /b 0
)

if exist "%VSDEVCMD_X86%" (
  echo [Iris Setup] Visual C++ Build Tools are installed and can be loaded via VsDevCmd.
  exit /b 0
)

if exist "%VSDEVCMD_X64%" (
  echo [Iris Setup] Visual C++ Build Tools are installed and can be loaded via VsDevCmd.
  exit /b 0
)

echo [Iris Setup] Visual C++ Build Tools missing. Installing required workload...
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-package-agreements --accept-source-agreements --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
if errorlevel 1 (
  echo [Iris Setup] Build Tools install failed.
  exit /b 1
)
exit /b 0
