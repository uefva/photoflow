@echo off
setlocal
where py >nul 2>nul
if %errorlevel%==0 (
  set "PY=py -3"
) else (
  set "PY=python"
)
if "%~1"=="" (
  %PY% "%~dp0photoflow.py" gui
) else (
  %PY% "%~dp0photoflow.py" %*
)
endlocal
