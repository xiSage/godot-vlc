cargo build

if ($IsWindows) {
    if (!(Test-Path "demo/addons/godot-vlc/bin/win64/")) {
        New-Item -Path "demo/addons/godot-vlc/bin/win64/" -ItemType Directory
    }
    Copy-Item `
        -Path "target/debug/godot_vlc.dll" `
        -Destination "demo/addons/godot-vlc/bin/win64/godot_vlc_debug.dll"`
        -Force
    Copy-Item `
        -Path "target/debug/godot_vlc.pdb" `
        -Destination "demo/addons/godot-vlc/bin/win64/godot_vlc_debug.pdb"`
        -Force
} elseif ($IsLinux) {
    if (!(Test-Path "demo/addons/godot-vlc/bin/linux64/")) {
        New-Item -Path "demo/addons/godot-vlc/bin/linux64/" -ItemType Directory
    }
    Copy-Item `
        -Path "target/debug/libgodot_vlc.so" `
        -Destination "demo/addons/godot-vlc/bin/linux64/libgodot_vlc_debug.so"`
        -Force
}

