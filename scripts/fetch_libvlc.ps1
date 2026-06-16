param(
    [string[]]$Platforms = @("win64")
)

$win64Url = "https://artifacts.videolan.org/vlc/nightly-win64/20260615-0427/vlc-4.0.0-dev-win64-853c0f19.7z"
$linux64Url = "https://github.com/xiSage/libvlc-4-linux-binary/releases/download/vlc_4.0.0-dev-37536-g546e18e53e_amd64/linux-x64.tar.gz"

if (!(Test-Path "thirdparty\vlc")) {
    New-Item -Path "thirdparty\vlc" -ItemType Directory -Force | Out-Null
}

if ($Platforms -contains "win64") {
    $archive = "vlc-win64.7z"
    $tempDir = "temp_vlc_extract"
    $targetDir = "thirdparty\vlc\win-x64"

    Invoke-WebRequest -Uri $win64Url -OutFile $archive

    # Clean up previous temp / target dirs
    if (Test-Path $tempDir) { Remove-Item -Recurse -Force $tempDir }
    if (Test-Path $targetDir) { Remove-Item -Recurse -Force $targetDir }

    New-Item -Path $targetDir -ItemType Directory -Force | Out-Null

    # Extract selected files from 7z
    & 7z x $archive -o"$tempDir" -y "vlc-4.0.0-dev\libvlc.dll" "vlc-4.0.0-dev\libvlccore.dll" "vlc-4.0.0-dev\plugins\*" "vlc-4.0.0-dev\sdk\*"

    # Move headers and libs
    Move-Item -Path "$tempDir\vlc-4.0.0-dev\sdk\include" -Destination "$targetDir" -Force
    Move-Item -Path "$tempDir\vlc-4.0.0-dev\sdk\lib" -Destination "$targetDir" -Force

    # Move DLLs and plugins to lib/
    Move-Item -Path "$tempDir\vlc-4.0.0-dev\libvlc.dll" -Destination "$targetDir\lib\" -Force
    Move-Item -Path "$tempDir\vlc-4.0.0-dev\libvlccore.dll" -Destination "$targetDir\lib\" -Force
    Move-Item -Path "$tempDir\vlc-4.0.0-dev\plugins" -Destination "$targetDir\lib\" -Force

    # Cleanup
    Remove-Item -Recurse -Force $tempDir
    Remove-Item -Force $archive
}

if ($Platforms -contains "linux64") {
    $archive = "vlc-linux64.tar.gz"
    $targetDir = "thirdparty/vlc"

    Invoke-WebRequest -Uri $linux64Url -OutFile $archive

    tar -zxvf"$archive" -C $targetDir

    # Cleanup
    Remove-Item -Force $archive
}
