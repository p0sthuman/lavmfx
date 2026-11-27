@echo off
rem LAVMFX troubleshooting: DirectX 12.
rem Starts LAVMFX with this setting. When it closes (or you end it in Task Manager),
rem the log is saved next to this file as test-1-dx12.log.
cd /d "%~dp0.."
set WGPU_BACKEND=dx12
echo Starting LAVMFX (DirectX 12).
echo Close it when you're done. If it freezes, end it in Task Manager.
lavmfx.exe
copy /y "%APPDATA%\lavmfx\data\lavmfx.log" "%~dp0test-1-dx12.log" >nul
echo Log saved: %~dp0test-1-dx12.log
pause
