@echo off
setlocal EnableExtensions DisableDelayedExpansion
cd /d "%~dp0" || goto error
echo.
echo ============================================
echo   ReviewFlow - Local Performance Review POC
echo ============================================
echo.

where powershell.exe >nul 2>nul
if errorlevel 1 (
  echo ERROR: Windows PowerShell is required for local configuration.
  goto error
)
if not exist "scripts\Prepare-ReviewFlow.ps1" (
  echo ERROR: Missing scripts\Prepare-ReviewFlow.ps1. Make sure you have the whole project folder.
  goto error
)
if "%RF_LOCAL_AI%"=="1" (
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File "scripts\Prepare-ReviewFlow.ps1" -ProjectRoot "%CD%" -LocalAI
) else (
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File "scripts\Prepare-ReviewFlow.ps1" -ProjectRoot "%CD%"
)
if errorlevel 1 (
  echo ERROR: Could not prepare the local .env configuration.
  goto error
)

rem Do not allow inherited environment values to override the repaired .env.
set "EMPLOYEE_API_TOKEN="
set "REVIEWER_API_TOKEN="
set "SQLITE_PATH="
set "BIND_ADDR="
set "AI_MODE="
set "OLLAMA_MODEL="
set "OLLAMA_BASE_URL="
set "AZURE_OPENAI_ENDPOINT="
set "AZURE_OPENAI_DEPLOYMENT="
set "AZURE_OPENAI_API_KEY="
for /f "usebackq eol=# tokens=1,* delims==" %%A in (".env") do (
  if /i "%%A"=="EMPLOYEE_API_TOKEN" set "EMPLOYEE_API_TOKEN=%%B"
  if /i "%%A"=="REVIEWER_API_TOKEN" set "REVIEWER_API_TOKEN=%%B"
  if /i "%%A"=="SQLITE_PATH" set "SQLITE_PATH=%%B"
  if /i "%%A"=="BIND_ADDR" set "BIND_ADDR=%%B"
  if /i "%%A"=="AI_MODE" set "AI_MODE=%%B"
  if /i "%%A"=="OLLAMA_MODEL" set "OLLAMA_MODEL=%%B"
  if /i "%%A"=="OLLAMA_BASE_URL" set "OLLAMA_BASE_URL=%%B"
  if /i "%%A"=="AZURE_OPENAI_ENDPOINT" set "AZURE_OPENAI_ENDPOINT=%%B"
  if /i "%%A"=="AZURE_OPENAI_DEPLOYMENT" set "AZURE_OPENAI_DEPLOYMENT=%%B"
  if /i "%%A"=="AZURE_OPENAI_API_KEY" set "AZURE_OPENAI_API_KEY=%%B"
)
if not defined EMPLOYEE_API_TOKEN goto env_error
if not defined REVIEWER_API_TOKEN goto env_error
if "%EMPLOYEE_API_TOKEN%"=="%REVIEWER_API_TOKEN%" goto env_error
if not defined BIND_ADDR set "BIND_ADDR=127.0.0.1:8080"
for /f "tokens=2 delims=:" %%P in ("%BIND_ADDR%") do set "PORT=%%P"
if not defined PORT (
  echo ERROR: Invalid BIND_ADDR. Example: 127.0.0.1:8080
  goto error
)
set "URL=http://127.0.0.1:%PORT%"

if /i "%AI_MODE%"=="ollama" (
  call :ollama_setup
  if errorlevel 1 goto error
)

rem Check for a running local ReviewFlow instance.
powershell.exe -NoProfile -Command "try { $x=Invoke-RestMethod -Uri '%URL%/healthz' -TimeoutSec 2; if ($x.status -eq 'ok' -and $x.workday -eq 'staging-only') { if ($x.ai_mode -eq $env:AI_MODE) {exit 0} else {exit 2} } } catch { }; exit 1" >nul 2>nul
if errorlevel 2 goto restart_required
if not errorlevel 1 goto browser

if "%RF_FORCE_DOCKER%"=="1" goto docker

set "CARGO=cargo"
where cargo.exe >nul 2>nul
if errorlevel 1 (
  set "CARGO="
  if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"
)
if defined CARGO goto rust
where docker.exe >nul 2>nul
if not errorlevel 1 goto docker

echo ERROR: Rust Cargo and Docker were not found.
echo Install Rust from https://rustup.rs/ or Docker Desktop, then retry.
echo Windows Rust may also require Microsoft C++ Build Tools.
goto error

:rust
rem Rust's Windows MSVC target requires Microsoft C++ Build Tools and the Windows SDK.
rem Detect this up front to avoid a cryptic "link.exe not found" compiler error.
set "RUSTC=rustc.exe"
where rustc.exe >nul 2>nul
if errorlevel 1 (
  if exist "%USERPROFILE%\.cargo\bin\rustc.exe" set "RUSTC=%USERPROFILE%\.cargo\bin\rustc.exe"
)
"%RUSTC%" -vV 2>nul | findstr /i /c:"windows-msvc" >nul
if not errorlevel 1 (
  rem Avoid a false MSVC dependency when the user explicitly builds the GNU target.
  echo %CARGO_BUILD_TARGET% | findstr /i /c:"windows-gnu" >nul
  if errorlevel 1 (
    call :init_msvc
    if errorlevel 1 (
      rem Docker can compile Rust in Linux without the Windows linker.
      rem We do not silently choose Docker for Ollama: its host networking needs extra setup.
      if /i not "%AI_MODE%"=="ollama" (
        where docker.exe >nul 2>nul
        if not errorlevel 1 (
          docker info >nul 2>nul
          if not errorlevel 1 (
            echo [ReviewFlow] MSVC linker missing; switching to Docker Desktop Linux build.
            goto docker
          )
        )
      )
      goto missing_msvc
    )
  )
)
echo [ReviewFlow] Building Rust app (first run downloads dependencies)...
"%CARGO%" build
if errorlevel 1 (
  echo ERROR: Rust build failed. See the output above.
  goto error
)
echo [ReviewFlow] Starting app in a separate terminal window...
start "ReviewFlow POC Server" /D "%CD%" "%CD%\target\debug\reviewflow.exe"
goto wait

:init_msvc
rem Match the MSVC environment to the Rust host target. On ARM64 Windows, rustup
rem installs aarch64-pc-windows-msvc, which needs ARM64 libraries; using the x64
rem environment (vcvars64) causes "unresolved external symbol memcpy" link errors.
set "VC_ARCH=x64"
set "VC_COMPONENT=Microsoft.VisualStudio.Component.VC.Tools.x86.x64"
"%RUSTC%" -vV 2>nul | findstr /i /c:"host: aarch64" >nul
if not errorlevel 1 (
  set "VC_ARCH=arm64"
  set "VC_COMPONENT=Microsoft.VisualStudio.Component.VC.Tools.ARM64"
)
rem If already running inside a VS Developer Command Prompt, the linker is ready.
where link.exe >nul 2>nul
if not errorlevel 1 goto msvc_ready
rem Use vswhere to find current installed VS versions (2022, 2026, etc.).
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "VS_INSTALL="
if exist "%VSWHERE%" (
  for /f "usebackq delims=" %%V in (`"%VSWHERE%" -latest -products * -requires %VC_COMPONENT% -property installationPath`) do set "VS_INSTALL=%%V"
)
if defined VS_INSTALL (
  if exist "%VS_INSTALL%\VC\Auxiliary\Build\vcvarsall.bat" (
    echo [ReviewFlow] Initializing Visual Studio C++ Build Tools for %VC_ARCH%...
    call "%VS_INSTALL%\VC\Auxiliary\Build\vcvarsall.bat" %VC_ARCH% >nul
    if not errorlevel 1 (
      where link.exe >nul 2>nul
      if not errorlevel 1 goto msvc_ready
    )
  )
)
rem Fallback for VS installations where vswhere was not installed or discoverable.
for %%V in (
  "%ProgramFiles%\Microsoft Visual Studio\2022\BuildTools"
  "%ProgramFiles%\Microsoft Visual Studio\2022\Community"
  "%ProgramFiles%\Microsoft Visual Studio\2022\Professional"
  "%ProgramFiles%\Microsoft Visual Studio\2022\Enterprise"
) do (
  if exist "%%~V\VC\Auxiliary\Build\vcvarsall.bat" (
    set "VS_INSTALL=%%~V"
    call "%%~V\VC\Auxiliary\Build\vcvarsall.bat" %VC_ARCH% >nul
    if not errorlevel 1 (
      where link.exe >nul 2>nul
      if not errorlevel 1 goto msvc_ready
    )
  )
)
exit /b 1

:msvc_ready
rem The "ring" crate (TLS via reqwest/rustls) needs clang to build on ARM64 Windows.
rem Visual Studio's "C++ Clang tools" component ships it, but not on PATH.
if /i not "%VC_ARCH%"=="arm64" exit /b 0
where clang.exe >nul 2>nul
if not errorlevel 1 exit /b 0
if defined VS_INSTALL (
  if exist "%VS_INSTALL%\VC\Tools\Llvm\ARM64\bin\clang.exe" set "PATH=%VS_INSTALL%\VC\Tools\Llvm\ARM64\bin;%PATH%"
)
where clang.exe >nul 2>nul
if not errorlevel 1 exit /b 0
echo [ReviewFlow] clang.exe not found; it is required on ARM64 Windows.
exit /b 1

:missing_msvc
echo.
echo =======================================================
echo   WINDOWS C++ BUILD TOOLS REQUIRED
 echo =======================================================
echo Rust is installed, but the MSVC linker link.exe is not available.
echo.
echo Fix (one-time installation):
echo   1. Open https://visualstudio.microsoft.com/downloads/
echo   2. Download "Build Tools for Visual Studio".
echo   3. Select "Desktop development with C++".
echo   4. Include MSVC x64/x86 build tools and a Windows SDK.
echo      On ARM64 Windows (Snapdragon / Surface Pro X, etc.) also select
echo      "MSVC ARM64 build tools" and "C++ Clang tools for Windows".
echo   5. Install, then double-click this launcher again.
echo.
echo Full Visual Studio is NOT required; VS Code alone is insufficient.
echo Alternative: Install/start Docker Desktop in Linux-container mode,
echo then run Run-ReviewFlow-Docker.bat (non-AI mode).
if /i "%AI_MODE%"=="ollama" (
  echo NOTE: For Ollama mode, native Rust + MSVC is recommended.
  echo Docker requires additional host-to-container Ollama configuration.
)
echo =======================================================
goto error

:docker
echo [ReviewFlow] Building with Docker (Linux toolchain; no link.exe required)...
where docker.exe >nul 2>nul
if errorlevel 1 (
  echo ERROR: Docker Desktop is not installed or is not on PATH.
  echo Install it from https://www.docker.com/products/docker-desktop/
  goto error
)
docker info >nul 2>nul
if errorlevel 1 (
  echo ERROR: Docker Desktop must be running.
  goto error
)
if /i "%AI_MODE%"=="ollama" (
  echo [ReviewFlow] NOTE: Docker + Ollama requires Ollama to listen beyond 127.0.0.1 on the host.
  echo [ReviewFlow] Prefer local Rust/Cargo for simplest offline AI setup.
  set "OLLAMA_BASE_URL=http://host.docker.internal:11434"
)
docker build -t reviewflow-poc:local .
if errorlevel 1 goto error
docker container inspect reviewflow-poc-local >nul 2>nul
if errorlevel 1 (
  docker run -d --rm --name reviewflow-poc-local -p 127.0.0.1:%PORT%:8080 -v reviewflow-poc-data:/data --env-file .env -e SQLITE_PATH=/data/reviewflow.db -e BIND_ADDR=0.0.0.0:8080 -e OLLAMA_BASE_URL=%OLLAMA_BASE_URL% reviewflow-poc:local
) else (
  docker start reviewflow-poc-local
)
if errorlevel 1 goto error
echo To stop Docker mode, run: docker stop reviewflow-poc-local
goto wait

:wait
echo [ReviewFlow] Checking server readiness...
powershell.exe -NoProfile -Command "$ready=$false; for ($i=0; $i -lt 90; $i++) { try { $x=Invoke-RestMethod -Uri '%URL%/healthz' -TimeoutSec 2; if ($x.status -eq 'ok' -and $x.workday -eq 'staging-only') { $ready=$true; break } } catch { }; Start-Sleep -Milliseconds 500 }; if ($ready) {exit 0} else {exit 1}"
if errorlevel 1 (
  echo ERROR: Server did not become ready. Check the server terminal or Docker logs.
  goto error
)
:browser
start "" "%URL%"
echo.
echo [ReviewFlow] Running at %URL%
echo Copy REVIEWER_API_TOKEN from .env into the web UI.
echo DEMO ONLY: Workday integration is simulated. Do not use actual employee data.
echo.
pause
exit /b 0

:ollama_setup
set "OLLAMA_EXE=ollama.exe"
where ollama.exe >nul 2>nul
if errorlevel 1 (
  set "OLLAMA_EXE="
  if exist "%LOCALAPPDATA%\Programs\Ollama\ollama.exe" set "OLLAMA_EXE=%LOCALAPPDATA%\Programs\Ollama\ollama.exe"
  if not defined OLLAMA_EXE if exist "%PROGRAMFILES%\Ollama\ollama.exe" set "OLLAMA_EXE=%PROGRAMFILES%\Ollama\ollama.exe"
)
if not defined OLLAMA_EXE (
  echo ERROR: Ollama is not installed. Download it from https://ollama.com/download
  exit /b 1
)
if not defined OLLAMA_MODEL set "OLLAMA_MODEL=qwen3:4b-instruct"
echo [ReviewFlow] Local AI model: %OLLAMA_MODEL%
powershell.exe -NoProfile -Command "try { Invoke-RestMethod -Uri 'http://127.0.0.1:11434/api/tags' -TimeoutSec 2;  exit 0 } catch { exit 1 }" >nul 2>nul
if errorlevel 1 (
  echo [ReviewFlow] Starting Ollama server...
  start "Ollama Local AI" /MIN "%OLLAMA_EXE%" serve
)
powershell.exe -NoProfile -Command "$ready=$false; for ($i=0; $i -lt 40; $i++) { try { Invoke-RestMethod -Uri 'http://127.0.0.1:11434/api/tags' -TimeoutSec 2;  $ready=$true; break } catch { }; Start-Sleep -Seconds 1 }; if ($ready) {exit 0} else {exit 1}" >nul 2>nul
if errorlevel 1 (
  echo ERROR: Ollama did not become available at http://127.0.0.1:11434
  exit /b 1
)
rem Download if absent. This is a one-time model download, about 2.5 GB.
"%OLLAMA_EXE%" list | findstr /i /l /c:"%OLLAMA_MODEL%" >nul
if errorlevel 1 (
  echo [ReviewFlow] Downloading %OLLAMA_MODEL% for first-time local AI use...
  "%OLLAMA_EXE%" pull "%OLLAMA_MODEL%"
  if errorlevel 1 (
    echo ERROR: Ollama model download failed.
    exit /b 1
  )
)
echo [ReviewFlow] Ollama is ready.
exit /b 0

:restart_required
echo ERROR: An older ReviewFlow app is already using %URL% with a different AI mode.
echo Close its server terminal or stop its Docker container, then run this launcher again.
goto error

:env_error
echo ERROR: Unable to read valid demo tokens from .env
echo Check the output from scripts\Prepare-ReviewFlow.ps1
goto error
:error
echo.
echo Launch could not complete. Review the message above.
pause
exit /b 1
