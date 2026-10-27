@echo off
rem Runs the prebuilt llm-wiki binary that ships inside this skill package.
rem
rem Windows counterpart of bin/llm-wiki: no compiler, no network, no PATH setup.
rem Install it once with bin\bootstrap.ps1 if you prefer bare `llm-wiki` calls.
setlocal
set "HERE=%~dp0"
set "BIN=%HERE%vendor\x86_64-pc-windows-msvc\llm-wiki.exe"

if not exist "%BIN%" (
  echo llm-wiki: package has no x86_64-pc-windows-msvc binary ^(looked in "%BIN%"^). 1>&2
  echo llm-wiki: run `scripts/vendor.sh x86_64-pc-windows-msvc ^<path-to-binary^>` where the package is built. 1>&2
  exit /b 2
)

"%BIN%" %*
exit /b %ERRORLEVEL%
