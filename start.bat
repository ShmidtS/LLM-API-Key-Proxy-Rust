@echo off
chcp 65001 >nul
setlocal

echo Starting LLM-API-Key-Proxy (Rust)...
echo.

if exist .env (
    for /f "usebackq eol=# tokens=1,* delims==" %%a in (".env") do (
        set "%%a=%%b"
    )
)

:: Set default log level if not already configured
if not defined RUST_LOG set RUST_LOG=proxy_app=debug,tower_http=debug

cargo run --release --bin proxy_app

if errorlevel 1 (
    echo.
    echo Build or runtime error occurred.
    pause
)

endlocal
