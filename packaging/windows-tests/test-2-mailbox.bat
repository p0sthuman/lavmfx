@echo off
rem LAVMFX troubleshooting: Vulkan, no vsync wait (mailbox).
rem Starts LAVMFX with this setting. When it closes (or you end it in Task Manager),
rem the log is saved next to this file as test-2-mailbox.log.
cd /d "%~dp0.."
set LAVMFX_PRESENT=mailbox
echo Starting LAVMFX (Vulkan, no vsync wait (mailbox)).
echo Close it when you're done. If it freezes, end it in Task Manager.
lavmfx.exe
copy /y "%APPDATA%\lavmfx\data\lavmfx.log" "%~dp0test-2-mailbox.log" >nul
echo Log saved: %~dp0test-2-mailbox.log
pause
