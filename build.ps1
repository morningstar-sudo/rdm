$ErrorActionPreference = 'Stop'
Push-Location $PSScriptRoot
try {
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo) {
        throw 'Cargo was not found. Install stable Rust for MSVC before building.'
    }

    $target = 'x86_64-pc-windows-msvc'
    & rustup target add $target
    if ($LASTEXITCODE -ne 0) {
        throw "Could not install the Rust target $target."
    }

    & $cargo.Source build --release --target $target --bins
    if ($LASTEXITCODE -ne 0) {
        throw 'RDM build failed. Visual Studio C++ Build Tools and the Windows SDK are required.'
    }

    $output = Join-Path $PSScriptRoot 'target\x86_64-pc-windows-msvc\release'
    Write-Output "Build complete: $output"
}
finally {
    Pop-Location
}
