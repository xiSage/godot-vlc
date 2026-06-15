param(
    [string[]]$Platforms = @("win64")
)

$win64Url = "https://artifacts.videolan.org/vlc/nightly-win64/20260615-0427/vlc-4.0.0-dev-win64-853c0f19.7z"

if ($Platforms -contains "win64") {
    $archive = "vlc-win64.7z"
    $tempDir = "temp_vlc_extract"
    $targetLibDir = "thirdparty\vlc\win-x64\lib"
    $targetIncludeDir = "thirdparty\vlc\win-x64\include"

    Invoke-WebRequest -Uri $win64Url -OutFile $archive

    # Clean up previous temp / target dirs
    if (Test-Path $tempDir) { Remove-Item -Recurse -Force $tempDir }
    if (Test-Path "thirdparty\vlc\win-x64") { Remove-Item -Recurse -Force "thirdparty\vlc\win-x64" }

    New-Item -Path $targetLibDir -ItemType Directory -Force | Out-Null
    New-Item -Path $targetIncludeDir -ItemType Directory -Force | Out-Null

    # Extract selected files from 7z
    & 7z x $archive -o"$tempDir" -y "vlc-4.0.0-dev\libvlc.dll" "vlc-4.0.0-dev\libvlccore.dll" "vlc-4.0.0-dev\plugins\*" "vlc-4.0.0-dev\sdk\include\*"

    # Move DLLs and plugins to lib/
    Move-Item -Path "$tempDir\vlc-4.0.0-dev\libvlc.dll" -Destination $targetLibDir -Force
    Move-Item -Path "$tempDir\vlc-4.0.0-dev\libvlccore.dll" -Destination $targetLibDir -Force
    Move-Item -Path "$tempDir\vlc-4.0.0-dev\plugins\*" -Destination "$targetLibDir\plugins\" -Force

    # Move headers to include/
    Move-Item -Path "$tempDir\vlc-4.0.0-dev\sdk\include\*" -Destination $targetIncludeDir -Force

    # Cleanup
    Remove-Item -Recurse -Force $tempDir
    Remove-Item -Force $archive
}
