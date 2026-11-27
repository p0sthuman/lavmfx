@echo off
rem LAVMFX troubleshooting: DirectX 12 + mailbox.
rem Starts LAVMFX with this setting. When it closes (or you end it in Task Manager),
rem the log is saved next to this file as test-3-dx12-mailbox.log.
cd /d "%~dp0.."
set WGPU_BACKEND=dx12
set LAVMFX_PRESENT=mailbox
echo Starting LAVMFX (DirectX 12 + mailbox).
echo Close it when you're done. If it freezes, end it in Task Manager.
lavmfx.exe
copy /y "%APPDATA%\lavmfx\data\lavmfx.log" "%~dp0test-3-dx12-mailbox.log" >nul
echo Log saved: %~dp0test-3-dx12-mailbox.log
pause
