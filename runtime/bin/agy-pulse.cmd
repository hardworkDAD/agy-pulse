@echo off
setlocal
node "%~dp0agy-pulse.js" %* 2>nul
if %ERRORLEVEL%==0 exit /b 0
if exist "%LocalAppData%\Programs\nodejs\node.exe" "%LocalAppData%\Programs\nodejs\node.exe" "%~dp0agy-pulse.js" %*
if %ERRORLEVEL%==0 exit /b 0
if exist "%ProgramFiles%\nodejs\node.exe" "%ProgramFiles%\nodejs\node.exe" "%~dp0agy-pulse.js" %*
