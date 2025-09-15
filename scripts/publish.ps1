. scripts/build_debug.ps1
. scripts/build_release.ps1
$manifest = cargo read-manifest | ConvertFrom-Json
$fileName = "$($manifest.name)_v$($manifest.version).zip"
# Compress-Archive -Path "demo/addons" -DestinationPath "$($manifest.name)_v$($manifest.version).zip" -Force
Remove-Item -Path $fileName -ErrorAction SilentlyContinue
[System.IO.Compression.ZipFile]::CreateFromDirectory("demo/addons", $fileName, [System.IO.Compression.CompressionLevel]::Optimal, $true)