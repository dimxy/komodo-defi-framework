@echo off
rem (c) Decker

echo Windows version and architecture:
ver
wmic os get osarchitecture

set MM_VERSION=%APPVEYOR_BUILD_VERSION%
echo MM_VERSION: %MM_VERSION%

echo [#1] Install Rust, build nanomsg, curl and pthreads ...
call marketmaker_build_depends.cmd

echo [#2] Build MM1 and MM2.

rem Increased verbosity here allows us to see the MM1 CMake logs.
cargo build -vv
