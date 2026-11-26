@echo off
setlocal
where py >nul 2>nul
if %errorlevel%==0 (
  py -3 "%~dp0photoflow.py" %*
) else (
  python "%~dp0photoflow.py" %*
)
endlocal
