@echo off
setlocal

set WORKSPACE=%~dp0..\..
cd /d "%WORKSPACE%"

echo Building Rusty Bridger release binary...
cargo build --release -p rusty-bridge-ui
if errorlevel 1 goto :error

mkdir dist\out 2>nul

echo Running NSIS installer creator...
cd dist\windows
makensis installer.nsi
if errorlevel 1 goto :error

echo.
echo Done. Installer: dist\out\RustyBridger-0.2.0-windows-setup.exe
goto :eof

:error
echo Build failed.
exit /b 1
