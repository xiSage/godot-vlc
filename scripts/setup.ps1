$demoBin = "demo/addons/godot-vlc/bin"
$libDir = "thirdparty/vlc/lib"

New-Item -Path "$demoBin/win64/godot_vlc.dll" -Value "../../../../../target/release/godot_vlc.dll" -ItemType SymbolicLink -Force
New-Item -Path "$demoBin/win64/godot_vlc_debug.dll" -Value "../../../../../target/debug/godot_vlc.dll" -ItemType SymbolicLink -Force
New-Item -Path "$demoBin/win64/godot_vlc_debug.pdb" -Value "../../../../../target/debug/godot_vlc.pdb" -ItemType SymbolicLink -Force
New-Item -Path "$demoBin/linux64/libgodot_vlc.so" -Value "../../../../../target/release/libgodot_vlc.so" -ItemType SymbolicLink -Force
New-Item -Path "$demoBin/linux64/libgodot_vlc_debug.so" -Value "../../../../../target/debug/libgodot_vlc.so" -ItemType SymbolicLink -Force

New-Item -Path "$demoBin/win64/libvlc.dll" -Value "../../../../../$libDir/win64/libvlc.dll" -ItemType SymbolicLink -Force
New-Item -Path "$demoBin/win64/libvlccore.dll" -Value "../../../../../$libDir/win64/libvlccore.dll" -ItemType SymbolicLink -Force
New-Item -Path "$demoBin/win64/plugins/" -Value "../../../../../$libDir/win64/plugins" -ItemType SymbolicLink -Force

foreach ($item in Get-ChildItem "$libDir/linux64") {
    New-Item -Path "$demoBin/linux64/$($item.Name)" -Value "../../../../../$libDir/linux64/$($item.Name)" -ItemType SymbolicLink -Force
}