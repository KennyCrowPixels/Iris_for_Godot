@echo off
setlocal
pushd "%~dp0"

set "BUILD_OUT_DIR=D:\Iris_for_Godot\Builds"

echo [Iris Build] [5%%] Bootstrapping dependencies...
call "%~dp0setup-windows.bat" --consumer --yes
if errorlevel 1 (
  echo [Iris Build] Setup failed. Cannot continue.
  popd
  exit /b 1
)

echo [Iris Build] [15%%] Preparing output directories...

if not exist "%BUILD_OUT_DIR%" mkdir "%BUILD_OUT_DIR%"

if exist "%BUILD_OUT_DIR%\*.exe" del /Q "%BUILD_OUT_DIR%\*.exe" >nul 2>nul
if exist "%BUILD_OUT_DIR%\*.msi" del /Q "%BUILD_OUT_DIR%\*.msi" >nul 2>nul

if exist "src-tauri\target\release\bundle\nsis\*.exe" del /Q "src-tauri\target\release\bundle\nsis\*.exe" >nul 2>nul
if exist "src-tauri\target\release\bundle\msi\*.msi" del /Q "src-tauri\target\release\bundle\msi\*.msi" >nul 2>nul

if exist "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat" (
  call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\VsDevCmd.bat" -arch=x64 -host_arch=x64 >nul
)

echo [Iris Build] [35%%] Building production installer bundles...
call npm run tauri:build
if errorlevel 1 (
  echo [Iris Build] Build failed.
  popd
  exit /b 1
)

echo [Iris Build] [80%%] Copying installers to %BUILD_OUT_DIR% ...
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
  echo [Iris Build] Build succeeded but no installer files were found.
  echo [Iris Build] Check src-tauri\target\release\bundle\ for generated artifacts.
  popd
  exit /b 1
)

echo.
echo [Iris Build] [100%%] Build complete. Installers were copied to:
echo   - %BUILD_OUT_DIR%
echo.
echo Recommended: keep only the latest installer filename on your Google Site
echo so users always download the newest manual update package.

popd
exit /b 0
