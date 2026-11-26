@echo off
setlocal

rem 带参数就是命令行用法，必须留控制台，否则预演清单和报错都看不见
if not "%~1"=="" goto cli

rem 不带参数：直接开图形界面。优先用无窗口的解释器，避免闪一个黑窗出来
where pyw >nul 2>nul
if %errorlevel%==0 (
  start "" pyw -3 "%~dp0photoflow.py" gui
  goto :eof
)
where pythonw >nul 2>nul
if %errorlevel%==0 (
  start "" pythonw "%~dp0photoflow.py" gui
  goto :eof
)
rem 这台机器没有无窗口版解释器，退回普通 python（会留一个黑窗）
where py >nul 2>nul
if %errorlevel%==0 (
  py -3 "%~dp0photoflow.py" gui
  goto :eof
)
python "%~dp0photoflow.py" gui
goto :eof

:cli
where py >nul 2>nul
if %errorlevel%==0 (
  py -3 "%~dp0photoflow.py" %*
) else (
  python "%~dp0photoflow.py" %*
)
endlocal
