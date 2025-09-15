<img src="icon.svg" alt="icon" width="128"/>

# godot-vlc
[![Dynamic JSON Badge](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fgodotengine.org%2Fasset-library%2Fapi%2Fasset%2F3766&query=%24.version_string&logo=godotengine&label=asset%20library&labelColor=333639)](https://godotengine.org/asset-library/asset/3766)

VLC extension for Godot. Supports Godot 4.3 and newer.

## How to use
Put media files into `res://` and they will be loaded as `VLCMedia`. Then you can play them with `VLCMediaPlayer` node.

You can alse use `VLCMedia.load_from_file()` to load media from disk or `VLCMedia.load_from_mrl()` to load media from a [media resource locator](https://wiki.videolan.org/Media_resource_locator).

There are some other features, such as subtitles and chapters, can be accessed through scripts. For more information, see the ingame documentation.

## Screenshot
<img src="img/screenshot.png" alt="screenshot">
