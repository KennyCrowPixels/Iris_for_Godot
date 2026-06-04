@echo off
setlocal
pushd "%~dp0"

set "BUILD_OUT_DIR=D:\Iris_for_Godot\Builds"
set "NPM_CMD=npm"
set "NODE_EXE=node"
set "NPM_CLI_JS="

if not exist "%BUILD_OUT_DIR%" mkdir "%BUILD_OUT_DIR%"

if exist "%BUILD_OUT_DIR%\*.exe" del /Q "%BUILD_OUT_DIR%\*.exe" >nul 2>nul
if exist "%BUILD_OUT_DIR%\*.msi" del /Q "%BUILD_OUT_DIR%\*.msi" >nul 2>nul

if exist "src-tauri\target\release\bundle\nsis\*.exe" del /Q "src-tauri\target\release\bundle\nsis\*.exe" >nul 2>nul
if exist "src-tauri\target\release\bundle\msi\*.msi" del /Q "src-tauri\target\release\bundle\msi\*.msi" >nul 2>nul

if exist "C:\Program Files\nodejs\node.exe" (
  set "NODE_EXE=C:\Program Files\nodejs\node.exe"
  set "PATH=C:\Program Files\nodejs;%PATH%"
)

if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
  set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
)

if exist "C:\Program Files\nodejs\node_modules\npm\bin\npm-cli.js" (
  set "NPM_CLI_JS=C:\Program Files\nodejs\node_modules\npm\bin\npm-cli.js"
)

where npm >nul 2>nul
if errorlevel 1 (
  if exist "C:\Program Files\nodejs\npm.cmd" (
    set "NPM_CMD=C:\Program Files\nodejs\npm.cmd"
  ) else (
    echo [Iris] npm was not found in PATH and fallback path was missing.
    echo [Iris] Install Node.js or add npm to PATH, then retry.
    exit /b 1
  )
)

if not exist "node_modules\cross-env\package.json" (
  echo [Iris] Installing npm dependencies...
  if defined NPM_CLI_JS (
    call "%NODE_EXE%" "%NPM_CLI_JS%" install
  ) else (
    call "%NPM_CMD%" install
  )
  if errorlevel 1 (
    echo [Iris] npm install failed.
    popd
    exit /b 1
  )
)

echo [Iris] Building production installer bundles...
if defined NPM_CLI_JS (
  call "%NODE_EXE%" "%NPM_CLI_JS%" run tauri:build
) else (
  call "%NPM_CMD%" run tauri:build
)
if errorlevel 1 (
  echo [Iris] Build failed.
  popd
  exit /b 1
)

echo [Iris] Copying installers to %BUILD_OUT_DIR% ...
set "COPIED=0"
for %%F in ("src-tauri\target\release\bundle\nsis\*.exe") do (
  if exist "%%~fF" (
    copy /Y "%%~fF" "%BUILD_OUT_DIR%\" >nul
    set "COPIED=1"
  )
)
for %%F in ("src-tauri\target\release\bundle\msi\*.msi") do (
  if exist "%%~fF" (
    copy /Y "%%~fF" "%BUILD_OUT_DIR%\" >nul
    set "COPIED=1"
  )
)

if "%COPIED%"=="0" (
  echo [Iris] Build succeeded but no installer files were found.
  echo [Iris] Check src-tauri\target\release\bundle\ for generated artifacts.
  popd
  exit /b 1
)

echo.
echo [Iris] Build complete. Installers were copied to:
echo   - %BUILD_OUT_DIR%
echo.
echo Recommended: keep only the latest installer filename on your Google Site
echo so users always download the newest manual update package.

popd
exit /b 0
