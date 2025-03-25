cargo build
Remove-Item -Path "target/debug/godot_vlc_debug.dll", "target/debug/godot_vlc_debug.pdb" -ErrorAction SilentlyContinue
Rename-Item -Path "target\debug\godot_vlc.dll" -NewName "godot_vlc_debug.dll"
Rename-Item -Path "target\debug\godot_vlc.pdb" -NewName "godot_vlc_debug.pdb"
Copy-Item `
    -Path "target/debug/godot_vlc_debug.dll", "target/debug/godot_vlc_debug.pdb" `
    -Destination "demo/addons/godot-vlc/bin/win64"