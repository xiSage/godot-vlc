@tool
class_name VideoStreamVLCFormatLoader extends ResourceFormatLoader

var extensions: PackedStringArray = ["avi", "mkv", "mpg", "mpeg", "vob", "mov", "flv", "mp4", "nsv", "ogm", "ogv", "oga", "rm", "wmv"]

func _get_recognized_extensions() -> PackedStringArray:
	return extensions

func _get_resource_type(path: String) -> String:
	var extension = path.get_extension()
	if extensions.has(extension):
		return "VideoStreamVLC"
	else:
		return ""

func _handles_type(type: StringName) -> bool:
	if type == "VideoStream":
		return true
	else:
		return false

func _load(path: String, _original_path: String, _use_sub_threads: bool, _cache_mode: int) -> Variant:
	var f := FileAccess.open(path, FileAccess.READ)
	if f == null:
		return ERR_CANT_OPEN
	var resource := VideoStreamVLC.new()
	resource.file = path
	return resource
