. scripts/build_debug.ps1
. scripts/build_release.ps1
$manifest = cargo read-manifest | ConvertFrom-Json
Compress-Archive -Path "demo/addons" -DestinationPath "$($manifest.name)_v$($manifest.version).zip" -Force